pub struct StagedCompilePlan;
impl StagedCompilePlan {
    pub fn cleanup(&self, permit: zccache_core::sync::PublicationReadPermit) {
        let _ = std::fs::remove_dir_all("private-output");
        drop(permit);
    }
}
pub struct Impostor;
impl Impostor {
    pub fn cleanup(&self) {
        let _ = std::fs::remove_dir_all("shared-cache");
    }
}
pub fn cleanup() {
    let _ = std::fs::remove_dir_all("shared-cache");
}

impl StagedCompilePlan {
    pub fn cc(&self, permit: zccache_core::sync::PublicationReadPermit) {
        drop(permit);
        let _ = std::fs::remove_dir_all("private-output");
    }
}
