//! Agent manager.

use super::agent::Agent;
use std::collections::HashMap;

/// Manages all agents.
pub struct AgentManager {
    agents: HashMap<String, Agent>,
    name_counter: HashMap<String, u32>,
}

impl AgentManager {
    /// Create a new agent manager.
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
            name_counter: HashMap::new(),
        }
    }

    /// Generate a unique agent ID.
    pub fn generate_id(&mut self) -> String {
        let mut generator = names::Generator::default();
        loop {
            let base_name = generator.next().unwrap_or_else(|| "agent".to_string());

            // Check if this name is already used
            if !self.agents.contains_key(&base_name) {
                return base_name;
            }

            // If used, append a counter
            let counter = self.name_counter.entry(base_name.clone()).or_insert(1);
            *counter += 1;
            let numbered_name = format!("{}-{}", base_name, counter);

            if !self.agents.contains_key(&numbered_name) {
                return numbered_name;
            }
        }
    }

    /// Add an agent.
    pub fn add(&mut self, agent: Agent) {
        self.agents.insert(agent.id.clone(), agent);
    }

    /// Get an agent by ID.
    pub fn get(&self, id: &str) -> Option<&Agent> {
        self.agents.get(id)
    }

    /// Get a mutable reference to an agent by ID.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Agent> {
        self.agents.get_mut(id)
    }

    /// Remove an agent by ID.
    pub fn remove(&mut self, id: &str) -> Option<Agent> {
        self.agents.remove(id)
    }

    /// List all agents.
    pub fn list(&self) -> impl Iterator<Item = &Agent> {
        self.agents.values()
    }

    /// Get the number of agents.
    pub fn len(&self) -> usize {
        self.agents.len()
    }

    /// Check if there are no agents.
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }
}

impl Default for AgentManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_id_uniqueness() {
        let mut manager = AgentManager::new();

        // Generate many IDs and verify they're all unique
        let mut ids = std::collections::HashSet::new();
        for _ in 0..100 {
            let id = manager.generate_id();
            assert!(ids.insert(id.clone()), "Generated duplicate ID: {}", id);
        }
    }

    #[test]
    fn test_generate_id_format() {
        let mut manager = AgentManager::new();
        let id = manager.generate_id();

        // Should be adjective-noun format (contains a hyphen)
        assert!(
            id.contains('-'),
            "ID should be adjective-noun format: {}",
            id
        );

        // Should be lowercase
        assert_eq!(id, id.to_lowercase(), "ID should be lowercase: {}", id);
    }
}
