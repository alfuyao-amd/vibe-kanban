pub mod error;
pub mod lead_agent;
pub mod middleware;
pub mod procedure_catalog;
pub mod procedure_runtime;
pub mod relay_pairing;
pub mod routes;
pub mod runtime;
pub mod startup;

// #[cfg(feature = "cloud")]
// type DeploymentImpl = vibe_kanban_cloud::deployment::CloudDeployment;
// #[cfg(not(feature = "cloud"))]
pub type DeploymentImpl = local_deployment::LocalDeployment;
