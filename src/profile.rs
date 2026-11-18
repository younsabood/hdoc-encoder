/// Zstandard settings for the page-layout chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompressionProfile {
    #[default]
    Archival,
    Balanced,
    UltraLight,
}

impl CompressionProfile {
    pub(crate) fn zstd_level(self) -> i32 {
        match self {
            Self::Archival => 22,
            Self::Balanced => 19,
            Self::UltraLight => 9,
        }
    }

    pub(crate) fn is_archival(self) -> bool {
        matches!(self, Self::Archival)
    }

    pub(crate) fn as_metadata(self) -> &'static str {
        match self {
            Self::Archival => "archival",
            Self::Balanced => "balanced",
            Self::UltraLight => "ultra-light",
        }
    }
}
