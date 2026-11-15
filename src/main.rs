use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use kg::SCHEMA_VERSION;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use tracing_subscriber::EnvFilter;

pub const MANIFEST_FILE_NAME: &str = "manifest.json";
pub const MANIFEST_MAGIC: &str = "KG_STORE";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub magic: String,
    pub schema_version: u16,
    pub generator: String,
}

impl Manifest {
    pub fn new(schema_version: u16) -> Self {
        Self {
            magic: MANIFEST_MAGIC.to_string(),
            schema_version,
            generator: format!("kg {}", env!("CARGO_PKG_VERSION")),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.magic != MANIFEST_MAGIC {
            bail!(
                "invalid knowledge graph magic: expected '{}', found '{}'",
                MANIFEST_MAGIC,
                self.magic
            );
        }
        if self.schema_version != SCHEMA_VERSION {
            bail!(
                "unsupported schema version: expected {}, found {}",
                SCHEMA_VERSION,
                self.schema_version
            );
        }
        Ok(())
    }
}

pub fn resolve_repo_dir(repo: &Path) -> Result<PathBuf> {
    let canonical = repo
        .canonicalize()
        .with_context(|| format!("repository path does not exist: {}", repo.display()))?;
    if !canonical.is_dir() {
        bail!(
            "repository path is not a directory: {}",
            canonical.display()
        );
    }
    Ok(canonical)
}

pub fn read_and_validate_manifest(kg_dir: &Path) -> Result<Manifest> {
    let manifest_path = kg_dir.join(MANIFEST_FILE_NAME);
    let meta = match std::fs::symlink_metadata(&manifest_path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "knowledge graph directory at {} is missing manifest ({})",
                kg_dir.display(),
                MANIFEST_FILE_NAME
            );
        }
        Err(e) => {
            return Err(e).with_context(|| {
                format!("cannot inspect manifest at {}", manifest_path.display())
            });
        }
    };

    if meta.file_type().is_symlink() {
        bail!(
            "manifest at {} must not be a symlink",
            manifest_path.display()
        );
    }
    if !meta.is_file() {
        bail!(
            "manifest at {} is not a regular file",
            manifest_path.display()
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode();
        if mode & 0o077 != 0 {
            tracing::warn!(
                "permissions on {} are permissive ({:04o}); recommended is 0600",
                manifest_path.display(),
                mode & 0o777
            );
        }
    }

    let contents = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read manifest at {}", manifest_path.display()))?;

    let manifest: Manifest = serde_json::from_str(&contents).with_context(|| {
        format!(
            "corrupted manifest at {}: invalid JSON",
            manifest_path.display()
        )
    })?;

    manifest
        .validate()
        .with_context(|| format!("invalid manifest at {}", manifest_path.display()))?;

    Ok(manifest)
}

#[derive(Debug, PartialEq, Eq)]
pub enum InitOutcome {
    Created { kg_dir: PathBuf, manifest: Manifest },
    AlreadyInitialized { kg_dir: PathBuf, manifest: Manifest },
}

pub fn init_kg_repository(repo: &Path) -> Result<InitOutcome> {
    let repo_dir = resolve_repo_dir(repo)?;
    let kg_dir = repo_dir.join(".kg");

    match std::fs::symlink_metadata(&kg_dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                bail!(".kg must not be a symlink: {}", kg_dir.display());
            }
            if !meta.is_dir() {
                bail!(".kg exists but is not a directory: {}", kg_dir.display());
            }

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = meta.permissions().mode();
                if mode & 0o077 != 0 {
                    tracing::warn!(
                        "permissions on {} are permissive ({:04o}); recommended is 0700",
                        kg_dir.display(),
                        mode & 0o777
                    );
                }
            }

            let manifest = read_and_validate_manifest(&kg_dir)?;
            Ok(InitOutcome::AlreadyInitialized { kg_dir, manifest })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&kg_dir).with_context(|| {
                format!("failed to create .kg directory at {}", kg_dir.display())
            })?;

            let manifest = Manifest::new(SCHEMA_VERSION);
            let manifest_bytes = serde_json::to_vec_pretty(&manifest)
                .context("failed to serialize repository manifest")?;

            let manifest_path = kg_dir.join(MANIFEST_FILE_NAME);
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }

            let mut file = options.open(&manifest_path).with_context(|| {
                format!("failed to create manifest at {}", manifest_path.display())
            })?;
            file.write_all(&manifest_bytes).with_context(|| {
                format!("failed to write manifest at {}", manifest_path.display())
            })?;

            Ok(InitOutcome::Created { kg_dir, manifest })
        }
        Err(e) => Err(e).with_context(|| format!("cannot inspect .kg at {}", kg_dir.display())),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum StatusOutcome {
    NotInitialized { repo_dir: PathBuf },
    Active { kg_dir: PathBuf, manifest: Manifest },
}

pub fn status_kg_repository(repo: &Path) -> Result<StatusOutcome> {
    let repo_dir = resolve_repo_dir(repo)?;
    let kg_dir = repo_dir.join(".kg");

    match std::fs::symlink_metadata(&kg_dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                bail!(".kg must not be a symlink: {}", kg_dir.display());
            }
            if !meta.is_dir() {
                bail!(".kg exists but is not a directory: {}", kg_dir.display());
            }

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = meta.permissions().mode();
                if mode & 0o077 != 0 {
                    tracing::warn!(
                        "permissions on {} are permissive ({:04o}); recommended is 0700",
                        kg_dir.display(),
                        mode & 0o777
                    );
                }
            }

            let manifest = read_and_validate_manifest(&kg_dir)?;
            Ok(StatusOutcome::Active { kg_dir, manifest })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(StatusOutcome::NotInitialized { repo_dir })
        }
        Err(e) => Err(e).with_context(|| format!("cannot inspect .kg at {}", kg_dir.display())),
    }
}

#[derive(Parser)]
#[command(
    name = "kg",
    author,
    version,
    about = "High-performance multi-language code knowledge graph engine",
    long_about = "Deterministic, embedded, incremental code intelligence engine for multi-language repositories."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new knowledge graph repository store (.kg)
    Init {
        /// Target repository path
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Display status and statistics of the knowledge graph
    Status {
        /// Target repository path
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Init { path } => match init_kg_repository(&path)? {
            InitOutcome::Created { kg_dir, .. } => {
                println!(
                    "Initialized empty knowledge graph repository at: {}",
                    kg_dir.display()
                );
            }
            InitOutcome::AlreadyInitialized { kg_dir, .. } => {
                println!(
                    "Knowledge graph already initialized at: {}",
                    kg_dir.display()
                );
            }
        },
        Commands::Status { path } => match status_kg_repository(&path)? {
            StatusOutcome::NotInitialized { repo_dir } => {
                println!(
                    "No knowledge graph found at: {}. Run 'kg init' first.",
                    repo_dir.display()
                );
            }
            StatusOutcome::Active { kg_dir, manifest } => {
                println!(
                    "Knowledge graph directory active at: {} (schema version: {})",
                    kg_dir.display(),
                    manifest.schema_version
                );
            }
        },
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_resolve_repo_dir_rejects_missing_path() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does_not_exist");
        let err = resolve_repo_dir(&missing).unwrap_err();
        assert!(err.to_string().contains("repository path does not exist"));
    }

    #[test]
    fn test_resolve_repo_dir_rejects_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("repo_file");
        std::fs::write(&file_path, "not a dir").unwrap();
        let err = resolve_repo_dir(&file_path).unwrap_err();
        assert!(
            err.to_string()
                .contains("repository path is not a directory")
        );
    }

    #[test]
    fn test_init_creates_kg_and_manifest_with_restrictive_permissions() {
        let dir = tempdir().unwrap();
        let outcome = init_kg_repository(dir.path()).unwrap();

        let kg_dir = match outcome {
            InitOutcome::Created { kg_dir, manifest } => {
                assert_eq!(manifest.magic, MANIFEST_MAGIC);
                assert_eq!(manifest.schema_version, SCHEMA_VERSION);
                kg_dir
            }
            InitOutcome::AlreadyInitialized { .. } => panic!("expected Created outcome"),
        };

        assert!(kg_dir.is_dir());
        let manifest_path = kg_dir.join(MANIFEST_FILE_NAME);
        assert!(manifest_path.is_file());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::symlink_metadata(&kg_dir)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                dir_mode & 0o777,
                0o700,
                "expected 0700 dir permissions, got {:04o}",
                dir_mode & 0o777
            );

            let file_mode = std::fs::symlink_metadata(&manifest_path)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                file_mode & 0o777,
                0o600,
                "expected 0600 file permissions, got {:04o}",
                file_mode & 0o777
            );
        }

        // Idempotent second init
        let outcome2 = init_kg_repository(dir.path()).unwrap();
        match outcome2 {
            InitOutcome::AlreadyInitialized {
                kg_dir: k2,
                manifest: m2,
            } => {
                assert_eq!(k2, kg_dir);
                assert_eq!(m2.schema_version, SCHEMA_VERSION);
            }
            InitOutcome::Created { .. } => panic!("expected AlreadyInitialized outcome"),
        }
    }

    #[test]
    fn test_init_and_status_reject_symlink_kg() {
        let dir = tempdir().unwrap();
        let target_dir = tempdir().unwrap();
        let kg_symlink = dir.path().join(".kg");

        #[cfg(unix)]
        std::os::unix::fs::symlink(target_dir.path(), &kg_symlink).unwrap();

        #[cfg(unix)]
        {
            let init_err = init_kg_repository(dir.path()).unwrap_err();
            assert!(init_err.to_string().contains(".kg must not be a symlink"));

            let status_err = status_kg_repository(dir.path()).unwrap_err();
            assert!(status_err.to_string().contains(".kg must not be a symlink"));
        }
    }

    #[test]
    fn test_init_and_status_reject_file_kg() {
        let dir = tempdir().unwrap();
        let kg_file = dir.path().join(".kg");
        std::fs::write(&kg_file, "regular file").unwrap();

        let init_err = init_kg_repository(dir.path()).unwrap_err();
        assert!(
            init_err
                .to_string()
                .contains(".kg exists but is not a directory")
        );

        let status_err = status_kg_repository(dir.path()).unwrap_err();
        assert!(
            status_err
                .to_string()
                .contains(".kg exists but is not a directory")
        );
    }

    #[test]
    fn test_status_reports_not_initialized() {
        let dir = tempdir().unwrap();
        let outcome = status_kg_repository(dir.path()).unwrap();
        match outcome {
            StatusOutcome::NotInitialized { repo_dir } => {
                assert_eq!(repo_dir, dir.path().canonicalize().unwrap());
            }
            StatusOutcome::Active { .. } => panic!("expected NotInitialized outcome"),
        }
    }

    #[test]
    fn test_status_active_after_init() {
        let dir = tempdir().unwrap();
        init_kg_repository(dir.path()).unwrap();

        let outcome = status_kg_repository(dir.path()).unwrap();
        match outcome {
            StatusOutcome::Active { kg_dir, manifest } => {
                assert_eq!(kg_dir, dir.path().canonicalize().unwrap().join(".kg"));
                assert_eq!(manifest.magic, MANIFEST_MAGIC);
                assert_eq!(manifest.schema_version, SCHEMA_VERSION);
            }
            StatusOutcome::NotInitialized { .. } => panic!("expected Active outcome"),
        }
    }

    #[test]
    fn test_status_detects_missing_manifest() {
        let dir = tempdir().unwrap();
        let kg = dir.path().join(".kg");
        std::fs::create_dir(&kg).unwrap();

        let err = status_kg_repository(dir.path()).unwrap_err();
        assert!(err.to_string().contains("missing manifest"));
    }

    #[test]
    fn test_status_rejects_symlink_manifest() {
        let dir = tempdir().unwrap();
        let kg = dir.path().join(".kg");
        std::fs::create_dir(&kg).unwrap();

        let outside_dir = tempdir().unwrap();
        let outside_manifest = outside_dir.path().join("ext_manifest.json");
        std::fs::write(&outside_manifest, "{}").unwrap();

        let manifest_path = kg.join(MANIFEST_FILE_NAME);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside_manifest, &manifest_path).unwrap();
            let err = status_kg_repository(dir.path()).unwrap_err();
            assert!(
                err.to_string().contains("manifest at")
                    && err.to_string().contains("must not be a symlink")
            );
        }
    }

    #[test]
    fn test_status_rejects_corrupted_manifest_json() {
        let dir = tempdir().unwrap();
        let kg = dir.path().join(".kg");
        std::fs::create_dir(&kg).unwrap();
        let manifest_path = kg.join(MANIFEST_FILE_NAME);
        std::fs::write(&manifest_path, "{ not valid json").unwrap();

        let err = status_kg_repository(dir.path()).unwrap_err();
        assert!(err.to_string().contains("corrupted manifest"));
    }

    #[test]
    fn test_status_rejects_invalid_magic() {
        let dir = tempdir().unwrap();
        let kg = dir.path().join(".kg");
        std::fs::create_dir(&kg).unwrap();
        let manifest_path = kg.join(MANIFEST_FILE_NAME);
        let bad_manifest = Manifest {
            magic: "WRONG_MAGIC".to_string(),
            schema_version: SCHEMA_VERSION,
            generator: "kg 0.1.0".to_string(),
        };
        std::fs::write(
            &manifest_path,
            serde_json::to_string(&bad_manifest).unwrap(),
        )
        .unwrap();

        let err = status_kg_repository(dir.path()).unwrap_err();
        assert!(format!("{err:#}").contains("invalid knowledge graph magic"));
    }

    #[test]
    fn test_status_rejects_unsupported_schema_version() {
        let dir = tempdir().unwrap();
        let kg = dir.path().join(".kg");
        std::fs::create_dir(&kg).unwrap();
        let manifest_path = kg.join(MANIFEST_FILE_NAME);
        let bad_manifest = Manifest {
            magic: MANIFEST_MAGIC.to_string(),
            schema_version: 999,
            generator: "kg 0.1.0".to_string(),
        };
        std::fs::write(
            &manifest_path,
            serde_json::to_string(&bad_manifest).unwrap(),
        )
        .unwrap();

        let err = status_kg_repository(dir.path()).unwrap_err();
        assert!(format!("{err:#}").contains("unsupported schema version"));
    }
}
