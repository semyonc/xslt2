#![allow(dead_code)] // the model is consumed by the engine adapter, not yet by the stub
//! Environment materialization: resolve a test case's `<environment>` (shared by name, inline,
//! or absent) into absolute paths the engine can open. Nothing is parsed here; the engine
//! decides what to load and when.

use std::path::{Path, PathBuf};

use crate::catalog::{Collation, EnvRef, Environment, Param, Source, TestCase, TestSet};

#[derive(Debug, Default)]
pub struct ResolvedEnvironment {
    pub name: Option<String>,
    pub sources: Vec<ResolvedSource>,
    pub stylesheets: Vec<ResolvedStylesheet>,
    pub schemas: Vec<ResolvedSchema>,
    pub resources: Vec<ResolvedResource>,
    pub collations: Vec<Collation>,
    pub collections: Vec<ResolvedCollection>,
    pub params: Vec<Param>,
}

#[derive(Debug, Clone)]
pub struct ResolvedSource {
    pub role: Option<String>,
    pub uri: Option<String>,
    pub path: Option<PathBuf>,
    pub content: Option<String>,
    pub validation: Option<String>,
    pub xml_version: Option<String>,
    pub streaming: bool,
    pub select: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedStylesheet {
    pub path: Option<PathBuf>,
    pub uri: Option<String>,
    pub role: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedSchema {
    pub path: Option<PathBuf>,
    pub uri: Option<String>,
    pub role: Option<String>,
    pub xsd_version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedResource {
    pub path: Option<PathBuf>,
    pub uri: Option<String>,
    pub media_type: Option<String>,
    pub encoding: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedCollection {
    pub uri: Option<String>,
    pub sources: Vec<ResolvedSource>,
}

impl ResolvedEnvironment {
    /// The source with `role="."`: the initial context item.
    pub fn context_source(&self) -> Option<&ResolvedSource> {
        self.sources.iter().find(|s| s.role.as_deref() == Some("."))
    }
}

pub fn resolve(case: &TestCase, set: &TestSet) -> Result<ResolvedEnvironment, String> {
    let env: Option<&Environment> =
        match &case.environment {
            None => None,
            Some(EnvRef::Inline(e)) => Some(e),
            Some(EnvRef::Ref(name)) => Some(set.environments.get(name).ok_or_else(|| {
                format!("environment '{}' not found in test-set {}", name, set.name)
            })?),
        };
    Ok(match env {
        None => ResolvedEnvironment::default(),
        Some(e) => materialize(e, &set.dir),
    })
}

fn resolve_path(dir: &Path, file: &Option<String>) -> Option<PathBuf> {
    file.as_ref().map(|f| dir.join(f))
}

fn resolve_source(s: &Source, dir: &Path) -> ResolvedSource {
    ResolvedSource {
        role: s.role.clone(),
        uri: s.uri.clone(),
        path: resolve_path(dir, &s.file),
        content: s.content.clone(),
        validation: s.validation.clone(),
        xml_version: s.xml_version.clone(),
        streaming: s.streaming,
        select: s.select.clone(),
    }
}

fn materialize(e: &Environment, dir: &Path) -> ResolvedEnvironment {
    ResolvedEnvironment {
        name: e.name.clone(),
        sources: e.sources.iter().map(|s| resolve_source(s, dir)).collect(),
        stylesheets: e
            .stylesheets
            .iter()
            .map(|s| ResolvedStylesheet {
                path: resolve_path(dir, &s.file),
                uri: s.uri.clone(),
                role: s.role.clone(),
            })
            .collect(),
        schemas: e
            .schemas
            .iter()
            .map(|s| ResolvedSchema {
                path: resolve_path(dir, &s.file),
                uri: s.uri.clone(),
                role: s.role.clone(),
                xsd_version: s.xsd_version.clone(),
            })
            .collect(),
        resources: e
            .resources
            .iter()
            .map(|r| ResolvedResource {
                path: resolve_path(dir, &r.file),
                uri: r.uri.clone(),
                media_type: r.media_type.clone(),
                encoding: r.encoding.clone(),
            })
            .collect(),
        collations: e.collations.clone(),
        collections: e
            .collections
            .iter()
            .map(|c| ResolvedCollection {
                uri: c.uri.clone(),
                sources: c.sources.iter().map(|s| resolve_source(s, dir)).collect(),
            })
            .collect(),
        params: e.params.clone(),
    }
}
