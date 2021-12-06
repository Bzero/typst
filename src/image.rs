//! Image handling.

use std::collections::{hash_map::Entry, HashMap};
use std::fmt::{self, Debug, Formatter};
use std::io;
use std::path::Path;
use std::rc::Rc;

use image::io::Reader as ImageReader;
use image::{DynamicImage, GenericImageView, ImageFormat};
use serde::{Deserialize, Serialize};
use usvg::{Error as USvgError, Tree};

use crate::loading::{FileHash, Loader};

/// A unique identifier for a loaded image.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ImageId(u32);

impl ImageId {
    /// Create an image id from the raw underlying value.
    ///
    /// This should only be called with values returned by
    /// [`into_raw`](Self::into_raw).
    pub const fn from_raw(v: u32) -> Self {
        Self(v)
    }

    /// Convert into the raw underlying value.
    pub const fn into_raw(self) -> u32 {
        self.0
    }
}

/// Storage for loaded and decoded images.
pub struct ImageStore {
    loader: Rc<dyn Loader>,
    files: HashMap<FileHash, ImageId>,
    images: Vec<Image>,
    on_load: Option<Box<dyn Fn(ImageId, &Image)>>,
}

impl ImageStore {
    /// Create a new, empty image store.
    pub fn new(loader: Rc<dyn Loader>) -> Self {
        Self {
            loader,
            files: HashMap::new(),
            images: vec![],
            on_load: None,
        }
    }

    /// Register a callback which is invoked each time an image is loaded.
    pub fn on_load<F>(&mut self, f: F)
    where
        F: Fn(ImageId, &Image) + 'static,
    {
        self.on_load = Some(Box::new(f));
    }

    /// Load and decode an image file from a path.
    pub fn load(&mut self, path: &Path) -> io::Result<ImageId> {
        let hash = self.loader.resolve(path)?;
        Ok(*match self.files.entry(hash) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let buffer = self.loader.load(path)?;
                let image = Image::parse(&buffer)?;
                let id = ImageId(self.images.len() as u32);
                if let Some(callback) = &self.on_load {
                    callback(id, &image);
                }
                self.images.push(image);
                entry.insert(id)
            }
        })
    }

    /// Get a reference to a loaded image.
    ///
    /// This panics if no image with this `id` was loaded. This function should
    /// only be called with ids returned by this store's [`load()`](Self::load)
    /// method.
    #[track_caller]
    pub fn get(&self, id: ImageId) -> &Image {
        &self.images[id.0 as usize]
    }
}

/// A loaded image.
#[derive(Debug)]
pub enum Image {
    Raster(RasterImage),
    Svg(Svg),
}

impl Image {
    /// Parse an image from raw data. This will prioritize SVG images and then
    /// try to decode a supported raster format.
    pub fn parse(data: &[u8]) -> io::Result<Self> {
        match Svg::parse(data) {
            Ok(svg) => Ok(Self::Svg(svg)),
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                Ok(Self::Raster(RasterImage::parse(data)?))
            }
            Err(e) => Err(e),
        }
    }

    /// The width of the image in pixels.
    pub fn width(&self) -> u32 {
        match self {
            Self::Raster(image) => image.width(),
            Self::Svg(image) => image.width(),
        }
    }

    /// The height of the image in pixels.
    pub fn height(&self) -> u32 {
        match self {
            Self::Raster(image) => image.height(),
            Self::Svg(image) => image.height(),
        }
    }

    pub fn is_vector(&self) -> bool {
        match self {
            Self::Raster(_) => false,
            Self::Svg(_) => true,
        }
    }
}

/// An SVG image, supported through the usvg crate.
pub struct Svg(pub Tree);

impl Svg {
    /// Parse an SVG file from a data buffer. This also handles `.svgz`
    /// compressed files.
    pub fn parse(data: &[u8]) -> io::Result<Self> {
        let usvg_opts = usvg::Options::default();
        let tree = Tree::from_data(data, &usvg_opts.to_ref()).map_err(|e| match e {
            USvgError::NotAnUtf8Str => {
                io::Error::new(io::ErrorKind::InvalidData, "file is not valid utf-8")
            }
            USvgError::MalformedGZip => io::Error::new(
                io::ErrorKind::InvalidData,
                "could not extract gzipped SVG",
            ),
            USvgError::ElementsLimitReached => io::Error::new(
                io::ErrorKind::Other,
                "SVG file has more than 1 million elements",
            ),
            USvgError::InvalidSize => io::Error::new(
                io::ErrorKind::InvalidData,
                "SVG width or height not greater than zero",
            ),
            USvgError::ParsingFailed(error) => io::Error::new(
                io::ErrorKind::InvalidData,
                format!("SVG parsing error: {}", error.to_string()),
            ),
        })?;

        Ok(Self(tree))
    }

    /// The width of the image in rounded-up nominal SVG pixels.
    pub fn width(&self) -> u32 {
        self.0.svg_node().size.width().ceil() as u32
    }

    /// The height of the image in rounded-up nominal SVG pixels.
    pub fn height(&self) -> u32 {
        self.0.svg_node().size.height().ceil() as u32
    }
}

impl Debug for Svg {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.debug_struct("Svg")
            .field("width", &self.0.svg_node().size.width())
            .field("height", &self.0.svg_node().size.height())
            .field("viewBox", &self.0.svg_node().view_box)
            .finish()
    }
}

/// A raster image, supported through the image crate.
pub struct RasterImage {
    /// The original format the image was encoded in.
    pub format: ImageFormat,
    /// The decoded image.
    pub buf: DynamicImage,
}

impl RasterImage {
    /// Parse an image from raw data in a supported format (PNG or JPEG).
    ///
    /// The image format is determined automatically.
    pub fn parse(data: &[u8]) -> io::Result<Self> {
        let cursor = io::Cursor::new(data);
        let reader = ImageReader::new(cursor).with_guessed_format()?;
        let format = reader.format().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "unknown image format")
        })?;

        let buf = reader
            .decode()
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

        Ok(Self { format, buf })
    }

    /// The width of the image.
    pub fn width(&self) -> u32 {
        self.buf.width()
    }

    /// The height of the image.
    pub fn height(&self) -> u32 {
        self.buf.height()
    }
}

impl Debug for RasterImage {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.debug_struct("Image")
            .field("format", &self.format)
            .field("color", &self.buf.color())
            .field("width", &self.width())
            .field("height", &self.height())
            .finish()
    }
}
