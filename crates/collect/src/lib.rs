//! Stage 1 — data collection from TrackDev and GitHub, plus local repo management.

pub mod collector;
pub mod github_client;
pub mod identity_resolver;
pub mod pm_client;
pub mod repo_manager;

pub use collector::{run_collection, CollectOpts};
pub use github_client::{GitHubClient, GitHubClientError};
pub use identity_resolver::{resolve_identities, IdentityResolverConfig, IdentityResolverStats};
pub use pm_client::{TrackDevClient, TrackDevError};
pub use repo_manager::{RepoError, RepoManager};
