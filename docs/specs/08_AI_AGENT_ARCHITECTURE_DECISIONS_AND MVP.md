# Final Architecture Decisions

## 1. Communication and Routing

- **Centralized Router Implementation**
  - All messages flow through central message bus
  - Redis for message queue and state management
  - Built-in monitoring and security checks
  - Clear audit trail of all agent communications

## 2. Agent and Task Management

- **Dynamic Agent Skill System**

```python
class AgentSkill(BaseModel):
    name: str
    description: str
    required_capabilities: List[str]
    prompt_template: str
    configuration: Dict[str, Any]

class SkillRegistry:
    def __init__(self):
        self.db = Database()  # MySQL connection
        self.skills: Dict[str, AgentSkill] = {}

    async def find_agent_for_task(
        self,
        task: Task
    ) -> Optional[Tuple[Agent, float]]:
        """Find best agent match for task, returns (agent, match_score)"""
        required_skills = await self.analyze_task_requirements(task)
        return await self.find_best_match(required_skills)

    async def propose_new_agent(
        self,
        required_skills: List[str]
    ) -> AgentConfig:
        """Generate config for new agent with required skills"""
        config = await self.generate_agent_config(required_skills)
        return AgentConfig(
            skills=required_skills,
            prompt=self.generate_prompt(required_skills),
            configuration=config
        )

    async def save_agent_config(
        self,
        config: AgentConfig,
        permanent: bool = False
    ) -> UUID:
        """Save agent configuration, optionally permanently"""
        if permanent:
            await self.db.save_agent_config(config)
        return await self.agent_manager.register_agent(config)
```

## 3. Security and Isolation

- **Process-Level Isolation Initially**
  - Each function runs in separate process
  - migrate to container-based isolation later
  - Bind mounts for file system access
  - Clear security boundaries

## 4. State and Storage

- **MySQL + Redis Architecture**

```python
class StateManager:
    def __init__(self):
        self.redis = Redis()  # Hot state
        self.db = MySQL()     # Persistent state

    async def get_state(self, key: str) -> Any:
        # Try Redis first
        if value := await self.redis.get(key):
            return value

        # Fall back to MySQL
        return await self.db.get_state(key)

    async def set_state(
        self,
        key: str,
        value: Any,
        persist: bool = False
    ):
        # Always set in Redis
        await self.redis.set(key, value)

        # Optionally persist to MySQL
        if persist:
            await self.db.set_state(key, value)
```

# MVP Scope Proposal

## Phase 1: Core Framework

1. **Basic Agent Management**

   - Central message router
   - Process-level isolation
   - Basic skill registry
   - Simple agent creation/destruction

2. **Task Management**

   - Task analysis and skill matching
   - Basic task queue
   - Simple task distribution

3. **Storage Layer**
   - MySQL for persistent storage
   - Redis for message bus and hot state
   - Basic state synchronization

## Phase 2: Agent Intelligence

1. **Skill System**

   - Skill definition and storage
   - Agent-skill matching
   - New agent proposal system

2. **Function Registry**
   - Basic bash command execution
   - Simple file system access
   - Process isolation

## Phase 3: Security & Monitoring

1. **Security Implementation**

   - Basic access controls
   - Process isolation
   - File system access controls

2. **Monitoring**
   - Basic logging
   - Simple metrics
   - Status dashboard

## Example Task Flow in MVP

```python
async def handle_task(task: Task) -> TaskResult:
    # 1. Analyze task requirements
    required_skills = await skill_registry.analyze_task(task)

    # 2. Find or create agent
    agent = await skill_registry.find_agent_for_task(task)
    if not agent:
        # Generate new agent config
        config = await skill_registry.propose_new_agent(required_skills)

        # Ask user permission
        if await request_user_permission(config):
            agent_id = await skill_registry.save_agent_config(
                config,
                permanent=await ask_save_permanent()
            )
            agent = await agent_manager.get_agent(agent_id)

    # 3. Execute task
    return await agent.execute_task(task)
```
