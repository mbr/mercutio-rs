//! Session storage for MCP servers.

use std::collections::HashMap;

use tokio::sync::{Mutex, RwLock};

use super::McpSessionId;
use crate::{McpServer, McpServerBuilder, ToolRegistry};

/// Session storage for MCP servers.
///
/// Each session has its own [`McpServer`] instance tracking protocol state. Sessions are created
/// on `initialize` requests and removed on `DELETE` or timeout.
pub struct Sessions<R: ToolRegistry> {
    /// Map of session ID to server instance.
    pub(super) servers: RwLock<HashMap<McpSessionId, Mutex<McpServer<R>>>>,
    /// Builder for creating new server instances.
    builder: McpServerBuilder<R>,
}

impl<R: ToolRegistry> Sessions<R> {
    /// Creates a new session store with the given server builder.
    ///
    /// The builder is used to create a fresh [`McpServer`] for each new session.
    pub fn new(builder: McpServerBuilder<R>) -> Self {
        Self {
            servers: RwLock::new(HashMap::new()),
            builder,
        }
    }

    /// Inserts a new session with the given ID.
    pub(super) async fn insert(&self, id: McpSessionId) {
        let server = self.builder.build();
        self.servers.write().await.insert(id, Mutex::new(server));
    }

    /// Removes a session by ID. Returns true if the session existed.
    pub async fn remove(&self, id: McpSessionId) -> bool {
        self.servers.write().await.remove(&id).is_some()
    }

    /// Returns the number of active sessions.
    pub async fn len(&self) -> usize {
        self.servers.read().await.len()
    }

    /// Returns true if there are no active sessions.
    pub async fn is_empty(&self) -> bool {
        self.servers.read().await.is_empty()
    }
}
