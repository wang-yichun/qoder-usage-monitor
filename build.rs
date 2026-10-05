use winres::{VersionInfo, WindowsResource};

fn main() {
    let version = env!("CARGO_PKG_VERSION");
    let mut resource = WindowsResource::new();
    resource
        .set("FileVersion", version)
        .set("ProductVersion", version)
        .set("ProductName", "Qoder Usage")
        .set("FileDescription", "Qoder 用量任务栏小组件")
        .set_version_info(VersionInfo::FILEVERSION, pack_version(version))
        .set_version_info(VersionInfo::PRODUCTVERSION, pack_version(version));
    if let Err(error) = resource.compile() {
        eprintln!("failed to embed Windows resources: {error}");
    }
}

fn pack_version(version: &str) -> u64 {
    let core = version.split('-').next().unwrap_or(version);
    let mut parts = core.split('.').map(|part| part.parse::<u64>().unwrap_or(0));
    let major = parts.next().unwrap_or(0).min(u16::MAX as u64);
    let minor = parts.next().unwrap_or(0).min(u16::MAX as u64);
    let patch = parts.next().unwrap_or(0).min(u16::MAX as u64);
    (major << 48) | (minor << 32) | (patch << 16)
}
