//! Reading and writing Roblox place and model files (.rbxl, .rbxlx, .rbxm, .rbxmx).

use std::fs;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use rbx_dom_weak::WeakDom;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFormat {
    BinaryPlace,
    XmlPlace,
    BinaryModel,
    XmlModel,
}

impl FileFormat {
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "rbxl" => Self::BinaryPlace,
            "rbxlx" => Self::XmlPlace,
            "rbxm" => Self::BinaryModel,
            "rbxmx" => Self::XmlModel,
            _ => return None,
        })
    }

    pub fn is_place(self) -> bool {
        matches!(self, Self::BinaryPlace | Self::XmlPlace)
    }

    pub fn is_xml(self) -> bool {
        matches!(self, Self::XmlPlace | Self::XmlModel)
    }
}

/// Returns true if the path looks like a place or model file Kahua can open.
pub fn is_roblox_file(path: &Path) -> bool {
    FileFormat::from_path(path).is_some()
}

/// Decodes a place or model from bytes, detecting binary vs XML by content.
pub fn decode(bytes: &[u8]) -> Result<WeakDom> {
    if bytes.starts_with(b"<roblox!") {
        rbx_binary::from_reader(bytes).context("failed to decode binary Roblox file")
    } else if bytes.iter().skip_while(|b| b.is_ascii_whitespace()).take(7).eq(b"<roblox".iter()) {
        rbx_xml::from_reader_default(bytes).context("failed to decode XML Roblox file")
    } else {
        bail!("not a Roblox place or model file")
    }
}

/// Reads a place or model file into a DOM. The DOM root is a `DataModel`
/// whose children are the services (for places) or top-level instances (for models).
pub fn read_file(path: &Path) -> Result<WeakDom> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    decode(&bytes).with_context(|| format!("failed to open {}", path.display()))
}

/// Encodes the children of the DOM root.
pub fn encode(dom: &WeakDom, xml: bool) -> Result<Vec<u8>> {
    let refs = dom.root().children().to_vec();
    encode_refs(dom, &refs, xml)
}

/// Encodes specific instances (and their descendants).
pub fn encode_refs(dom: &WeakDom, refs: &[rbx_dom_weak::types::Ref], xml: bool) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    if xml {
        rbx_xml::to_writer_default(&mut out, dom, refs).context("failed to encode XML")?;
    } else {
        rbx_binary::to_writer(&mut out, dom, refs).context("failed to encode binary")?;
    }
    Ok(out)
}

/// Writes the DOM to a file, choosing binary or XML from the extension.
/// The write is atomic: a temporary file is written and renamed into place.
pub fn write_file(dom: &WeakDom, path: &Path) -> Result<()> {
    let format = FileFormat::from_path(path).with_context(|| {
        format!("unknown file type for {} (expected .rbxl, .rbxlx, .rbxm or .rbxmx)", path.display())
    })?;
    let bytes = encode(dom, format.is_xml())?;
    write_atomic(path, &bytes)
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let file_name = path.file_name().context("path has no file name")?.to_string_lossy();
    let tmp = path.with_file_name(format!(".{file_name}.kahua-tmp"));
    {
        let file = fs::File::create(&tmp).with_context(|| format!("failed to create {}", tmp.display()))?;
        let mut writer = BufWriter::new(file);
        writer.write_all(bytes)?;
        writer.flush()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}
