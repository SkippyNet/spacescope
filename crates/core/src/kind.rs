//! File categories derived from the extension. Used for colouring.

pub const OTHER: u8 = 0;
pub const VIDEO: u8 = 1;
pub const AUDIO: u8 = 2;
pub const IMAGE: u8 = 3;
pub const DOCUMENT: u8 = 4;
pub const ARCHIVE: u8 = 5;
pub const CODE: u8 = 6;
pub const PROGRAM: u8 = 7;
pub const DATA: u8 = 8;
pub const COUNT: usize = 9;

pub const NAMES: [&str; COUNT] = [
    "Other",
    "Video",
    "Audio",
    "Images",
    "Documents",
    "Archives",
    "Code",
    "Programs",
    "Data",
];

/// Classifies a file by its name.
pub fn categorize(name: &[u8]) -> u8 {
    let Some(dot) = name.iter().rposition(|&b| b == b'.') else {
        return OTHER;
    };
    let ext = &name[dot + 1..];
    // Versioned shared objects: libfoo.so.1.2.3
    if !ext.is_empty()
        && ext.iter().all(|b| b.is_ascii_digit())
        && name.windows(4).any(|w| w == b".so.")
    {
        return PROGRAM;
    }
    if dot == 0 || ext.is_empty() || ext.len() > 12 {
        return OTHER;
    }
    let mut buf = [0u8; 12];
    for (d, s) in buf.iter_mut().zip(ext) {
        *d = s.to_ascii_lowercase();
    }
    match &buf[..ext.len()] {
        b"mp4" | b"mkv" | b"avi" | b"mov" | b"wmv" | b"webm" | b"flv" | b"m4v" | b"mpg"
        | b"mpeg" | b"m2ts" | b"vob" | b"3gp" | b"ogv" | b"mts" | b"braw" | b"mxf" => VIDEO,
        b"mp3" | b"flac" | b"wav" | b"aac" | b"ogg" | b"m4a" | b"wma" | b"opus" | b"aiff"
        | b"aif" | b"mid" | b"midi" | b"ape" | b"alac" | b"m4b" => AUDIO,
        b"jpg" | b"jpeg" | b"png" | b"gif" | b"bmp" | b"tif" | b"tiff" | b"webp" | b"heic"
        | b"heif" | b"raw" | b"cr2" | b"cr3" | b"nef" | b"arw" | b"dng" | b"orf" | b"rw2"
        | b"psd" | b"psb" | b"svg" | b"ico" | b"avif" | b"xcf" | b"ai" | b"eps" | b"tga"
        | b"dds" | b"exr" | b"hdr" | b"jxl" | b"kra" => IMAGE,
        b"pdf" | b"doc" | b"docx" | b"xls" | b"xlsx" | b"ppt" | b"pptx" | b"odt" | b"ods"
        | b"odp" | b"rtf" | b"txt" | b"md" | b"epub" | b"mobi" | b"azw3" | b"djvu" | b"tex"
        | b"pages" | b"numbers" | b"key" | b"csv" | b"one" | b"msg" | b"eml" | b"pst" | b"ost"
        | b"xps" | b"chm" => DOCUMENT,
        b"zip" | b"rar" | b"7z" | b"tar" | b"gz" | b"tgz" | b"bz2" | b"xz" | b"zst" | b"lz4"
        | b"lz" | b"cab" | b"deb" | b"rpm" | b"jar" | b"war" | b"apk" | b"xapk" | b"ipa"
        | b"whl" | b"nupkg" | b"crate" | b"gem" | b"txz" | b"tbz2" | b"z" | b"arj" | b"lzma"
        | b"appx" | b"msix" | b"snap" | b"flatpak" | b"appimage" | b"cpio" => ARCHIVE,
        b"rs" | b"c" | b"h" | b"cpp" | b"hpp" | b"cc" | b"cxx" | b"cs" | b"java" | b"kt"
        | b"py" | b"pyc" | b"pyd" | b"js" | b"mjs" | b"cjs" | b"jsx" | b"ts" | b"tsx" | b"go"
        | b"rb" | b"php" | b"swift" | b"m" | b"mm" | b"scala" | b"lua" | b"pl" | b"sh"
        | b"bash" | b"zsh" | b"ps1" | b"bat" | b"cmd" | b"html" | b"htm" | b"css" | b"scss"
        | b"less" | b"vue" | b"svelte" | b"json" | b"yaml" | b"yml" | b"toml" | b"xml" | b"sql"
        | b"r" | b"dart" | b"ex" | b"exs" | b"hs" | b"clj" | b"fs" | b"vb" | b"asm" | b"s"
        | b"cmake" | b"gradle" | b"ipynb" | b"map" | b"lock" | b"rlib" | b"rmeta" | b"o"
        | b"obj" | b"a" | b"lib" | b"pdb" | b"ilk" | b"class" | b"d" | b"wasm" | b"proto"
        | b"zig" | b"nim" | b"jl" | b"erl" => CODE,
        b"exe" | b"dll" | b"so" | b"dylib" | b"sys" | b"msi" | b"msp" | b"msu" | b"com"
        | b"scr" | b"drv" | b"ocx" | b"cpl" | b"efi" | b"ko" | b"node" | b"mui" | b"bin"
        | b"app" | b"elf" | b"run" | b"out" | b"winmd" | b"mum" | b"cat" | b"manifest" | b"ttf"
        | b"otf" | b"ttc" | b"woff" | b"woff2" | b"fon" => PROGRAM,
        b"db" | b"sqlite" | b"sqlite3" | b"mdb" | b"accdb" | b"mdf" | b"ldf" | b"ndf" | b"dat"
        | b"log" | b"iso" | b"img" | b"vhd" | b"vhdx" | b"vmdk" | b"vdi" | b"qcow2" | b"wim"
        | b"esd" | b"dmp" | b"bak" | b"tmp" | b"cache" | b"pak" | b"vpk" | b"wad" | b"bundle"
        | b"assets" | b"unity3d" | b"upk" | b"uasset" | b"umap" | b"pck" | b"idx" | b"pack"
        | b"etl" | b"evtx" | b"swp" | b"dmg" | b"blob" | b"parquet" | b"h5" | b"hdf5" | b"npy"
        | b"npz" | b"pkl" | b"pt" | b"pth" | b"onnx" | b"gguf" | b"safetensors" | b"ckpt"
        | b"tfrecord" | b"arrow" | b"feather" | b"mmdb" | b"edb" | b"jrs" | b"chk" | b"sdb"
        | b"reg" | b"hiv" | b"ldb" | b"sst" | b"wal" | b"journal" | b"ost2" | b"rdb" | b"aof"
        | b"sav" | b"rpf" | b"forge" | b"arc" | b"big" | b"bsa" | b"ba2" | b"pkg" | b"store"
        | b"vmem" | b"nvram" => DATA,
        _ => OTHER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn categories() {
        assert_eq!(categorize(b"movie.MKV"), VIDEO);
        assert_eq!(categorize(b"a.tar.gz"), ARCHIVE);
        assert_eq!(categorize(b".bashrc"), OTHER);
        assert_eq!(categorize(b"noext"), OTHER);
        assert_eq!(categorize(b"lib.rs"), CODE);
        assert_eq!(categorize(b"trailing."), OTHER);
        assert_eq!(categorize(b"libLLVM.so.20.1"), PROGRAM);
        assert_eq!(categorize(b"notes.2024"), OTHER);
        assert_eq!(categorize(b"model.safetensors"), DATA);
    }
}
