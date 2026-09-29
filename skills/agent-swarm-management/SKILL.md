# Agent Swarm Management Pattern

**category**: software-development
**tags**: agents, swarm, MCP, orchestration, task-management
**description**: Design and implementation patterns for managing clusters of AI agents with dynamic roles and responsibilities

## Overview

Agent swarms enable coordinated multi-agent workflows where individual agents can specialize, delegate, and collaborate. ModelMistress implements a master-agent pattern with MCP-based communication.

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    Swarm Controller                     │
│                    (ModelMistress)                       │
├─────────────────────────────────────────────────────────┤
│  Agent Registry    |  Task Queue  |  Event Bus         │
│  - AgentList       |  - Pending   |  - Pub/Sub         │
│  - Capabilities    |  - Running   |  - Subscriptions   │
│  - Status          |  - Completed |                   │
└─────────────────────────────────────────────────────────┘
         ▲              ▲              ▲
         │              │              │
    ┌────┴────┐   ┌─────┴─────┐    ┌───┴────┐
    │  Agent  │   │  Agent    │    │  Agent │
    │  A      │   │  B        │    │  C     │
    │(LLM)    │   │(Tool)     │    │(Code)  │
    └─────────┘   └───────────┘    └────────┘
```

## Agent Registry

### Agent Metadata

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub capabilities: Vec<String>,
    pub status: AgentStatus,
    pub location: AgentLocation,
    pub last_heartbeat: DateTime<Utc>,
    pub metadata: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentStatus {
    Idle,
    Busy,
    Searching,
    Waiting,
    Error(String),
    Offline,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentLocation {
    #[serde(rename = "type")]
    pub agent_type: AgentType,
    pub endpoint: Option<String>,
    pub mcp_server: Option<String>,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentType {
    LLM,       // Language model agent
    Tool,      // Tool-using agent
    Code,      // Code generator agent
    Planner,   // Task planner agent
    Coordinator, // Swarm coordinator
    Custom(String),
}
```

### Registry Operations

```rust
pub struct AgentRegistry {
    agents: DashMap<String, AgentInfo>,
    by_capability: DashMap<String, Vec<String>>,
    by_status: DashMap<AgentStatus, Vec<String>>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self {
            agents: DashMap::new(),
            by_capability: DashMap::new(),
            by_status: DashMap::new(),
        }
    }
    
    pub fn register(&mut self, agent: AgentInfo) -> Result<(), RegistryError> {
        // Check for duplicates
        if self.agents.contains_key(&agent.id) {
            return Err(RegistryError::AgentExists(agent.id.clone()));
        }
        
        // Index by capacity
        for cap in &agent.capabilities {
            self.by_capability
                .entry(cap.clone())
                .or_default()
                .push(agent.id.clone());
        }
        
        // Index by status
        self.by_status
            .entry(agent.status.clone())
            .or_default()
            .push(agent.id.clone());
        
        self.agents.insert(agent.id.clone(), agent);
        
        Ok(())
    }
    
    pub fn find_by_capability(&self, capability: &str) -> Vec<AgentInfo> {
        self.by_capability
            .get(capability)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.agents.get(id).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }
    
    pub fn update_status(&mut self, agent_id: &str, status: AgentStatus) -> Result<(), RegistryError> {
        if let Some(mut agent) = self.agents.get_mut(agent_id) {
            // Remove from old status index
            let old_status = agent.status.clone();
            self.by_status
                .get_mut(&old_status)
                .map(|ids| ids.retain(|id| id != agent_id));
            
            // Update status
            agent.status = status.clone();
            
            // Add to new status index
            self.by_status
                .entry(status)
                .or_default()
                .push(agent_id.to_string());
            
            Ok(())
        } else {
            Err(RegistryError::AgentNotFound(agent_id.to_string()))
        }
    }
}
```

## Task Management

### Task Specification

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub type_id: String,
    pub agent: Option<String>,
    pub intent: TaskIntent,
    pub constraints: TaskConstraints,
    pub priority: TaskPriority,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<TaskError>,
    pub result: Option<TaskResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskIntent {
    LoadModel { path: String, config: ModelConfig },
    Inference { model: String, prompt: String, max_tokens: usize },
    AgentSwarm { plan: String, agents: Vec<String> },
    Custom { name: String, args: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskConstraints {
    pub timeout: Option<Duration>,
    pub retry_count: usize,
    pub retry_delay: Duration,
    pub required_capabilities: Vec<String>,
    pub preemption: bool,
}
```

### Task Queue

```rust
pub struct TaskQueue {
    pending: Vec<TaskSpec>,
    running: HashMap<String, TaskSpec>,
    completed: VecDeque<TaskResult>,
    failed: VecDeque<TaskError>,
    max_completed: usize,
}

impl TaskQueue {
    pub fn new(max_completed: usize) -> Self {
        Self {
            pending: Vec::new(),
            running: HashMap::new(),
            completed: VecDeque::new(),
            failed: VecDeque::new(),
            max_completed,
        }
    }
    
    pub fn enqueue(&mut self, task: TaskSpec) {
        // Insert in priority order
        let insert_pos = self.pending
            .iter()
            .position(|t| task.priority > t.priority)
            .unwrap_or(self.pending.len());
        
        self.pending.insert(insert_pos, task);
    }
    
    pub fn next_task(&mut self, capabilities: &[String]) -> Option<TaskSpec> {
        // Find highest priority task that can be handled
        let pos = self.pending
            .iter()
            .position(|t| t.requires_capabilities(capabilities))?;
        
        Some(self.pending.remove(pos))
    }
    
    pub fn complete_task(&mut self, task_id: String, result: TaskResult) {
        if let Some(task) = self.running.remove(&task_id) {
            self.completed.push_back(result);
            
            // Trim old results
            while self.completed.len() > self.max_completed {
                self.completed.pop_front();
            }
            
            // Mark parent as complete if all children done
            if let Some(parent_id) = task.parent_id {
                self.check_parent_completion(&parent_id).await;
            }
        }
    }
}
```

## Coordination Patterns

### Master-Worker Pattern

```rust
pub struct SwarmCoordinator {
    registry: AgentRegistry,
    task_queue: TaskQueue,
    event_bus: EventBus,
}

impl SwarmCoordinator {
    pub async fn dispatch_task(&mut self, task: TaskSpec) -> Result<TaskResult, CoordinationError> {
        // Find suitable agent
        let candidates = self.registry.find_by_capability(&task.type_id);
        
        // Select best agent based on availability and capability match
        let agent = candidates
            .iter()
            .filter(|a| matches!(a.status, AgentStatus::Idle))
            .min_by_key(|a| a.last_heartbeat)
            .cloned();
        
        if let Some(agent) = agent {
            // Dispatch task
            let result = self.dispatch_to_agent(&agent, task.clone()).await?;
            
            // Update status
            self.registry.update_status(&agent.id, AgentStatus::Idle)?;
            
            Ok(result)
        } else {
            Err(CoordinationError::NoAgentAvailable)
        }
    }
    
    async fn dispatch_to_agent(&self, agent: &AgentInfo, task: TaskSpec) -> Result<TaskResult, CoordinationError> {
        match &agent.location {
            AgentLocation::MCP { server } => {
                // Dispatch via MCP
                let client = McpClient::connect(server).await?;
                client.call_tool("task:execute", serde_json::to_value(&task)?).await?
            }
            _ => Err(CoordinationError::UnsupportedTransport),
        }
    }
}
```

### Fanout Pattern

```rust
pub async fn fanout_task<T, U, F>(&self, task: T, agents: Vec<U>, processor: F) -> Vec<TaskResult>
where
    T: Clone + Send + Sync + Serialize + for<'de> Deserialize<'de>,
    U: Into<String> + Clone + Send + Sync,
    F: Fn(T, U) -> Future<Output = Result<TaskResult, TaskError>> + Send + Sync,
{
    let mut tasks = Vec::new();
    
    for agent_id in agents {
        let task_clone = task.clone();
        tasks.push(tokio::spawn(async move {
            processor(task_clone, agent_id.into()).await
        }));
    }
    
    let mut results = Vec::new();
    for task in tasks {
        match task.await {
            Ok(Ok(result)) => results.push(result),
            Ok(Err(e)) => results.push(TaskResult::error(e)),
            Err(e) => results.push(TaskResult::error(TaskError::JoinError(e))),
        }
    }
    
    results
}
```

## Event Bus

```rust
pub struct EventBus {
    subscribers: DashMap<String, Vec<TxEvent>>,
}

pub enum Event {
    AgentRegistered(AgentInfo),
    AgentStatusChanged { agent_id: String, status: AgentStatus },
    TaskCreated(TaskSpec),
    TaskCompleted { task_id: String, result: TaskResult },
    TaskFailed { task_id: String, error: TaskError },
}

impl EventBus {
    pub fn publish(&self, event: Event) {
        let channel = self.event_channel(&event);
        for tx in self.subscribers.get(&channel).into_iter().flat_map(|v| v.iter()) {
            let _ = tx.send(event.clone());
        }
    }
    
    pub fn subscribe(&self, event_type: &str) -> RxEvent {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.subscribers
            .entry(event_type.to_string())
            .or_default()
            .push(tx);
        rx
    }
}
```

## Best Practices

1. **Graceful degradation**: Continue operating when agents fail
2. **Heartbeat monitoring**: Detect offline agents quickly
3. **Capability-aware routing**: Match tasks to best agents
4. **Parallel processing**: Use fanout for embarrassingly parallel tasks
5. **Deadlock prevention**: Timeout and retry mechanisms
6. **Load balancing**: Distribute tasks based on agent capacity
7. **Audit trail**: Log all task executions for debugging