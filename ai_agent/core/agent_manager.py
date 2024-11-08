# ai_agent/core/agent_manager.py
"""
Agent lifecycle and management implementation.
Manages agent creation, termination, and coordination.
"""
import asyncio
import logging
from typing import Dict, List, Optional, Set
from uuid import UUID, uuid4

from pydantic import BaseModel, Field

from .message_bus import Message, MessageBus

logger = logging.getLogger(__name__)


class AgentCapability(BaseModel):
    """Define agent capabilities."""

    name: str
    description: str
    required_permissions: Set[str]


class AgentConfig(BaseModel):
    """Configuration for agent initialization."""

    name: str
    type: str
    capabilities: List[AgentCapability]
    max_concurrent_tasks: int = 1
    timeout_seconds: int = 300


class AgentStatus(BaseModel):
    """Current status of an agent."""

    is_active: bool = True
    current_tasks: List[UUID] = Field(default_factory=list)
    last_heartbeat: Optional[float] = None
    error_count: int = 0


class Agent(BaseModel):
    """Agent instance representation."""

    id: UUID = Field(default_factory=uuid4)
    config: AgentConfig
    status: AgentStatus = Field(default_factory=AgentStatus)


class AgentManager:
    """Manages agent lifecycle and coordination."""

    def __init__(self, message_bus: MessageBus):
        """Initialize agent manager."""
        self.message_bus = message_bus
        self.agents: Dict[UUID, Agent] = {}
        self._heartbeat_interval = 30.0  # seconds

    async def create_agent(self, config: AgentConfig) -> UUID:
        """
        Create a new agent instance.

        Args:
            config: Agent configuration

        Returns:
            UUID: ID of created agent
        """
        agent = Agent(config=config)
        self.agents[agent.id] = agent

        # Subscribe to agent's channel
        await self.message_bus.subscribe(
            f"agent.{agent.id}", self._handle_agent_message
        )

        logger.info(f"Created agent {agent.id} of type {config.type}")
        return agent.id

    async def get_agent(self, agent_id: UUID) -> Optional[Agent]:
        """Get agent by ID."""
        return self.agents.get(agent_id)

    async def terminate_agent(self, agent_id: UUID) -> bool:
        """
        Terminate an agent instance.

        Args:
            agent_id: ID of agent to terminate

        Returns:
            bool: True if agent was terminated
        """
        if agent_id not in self.agents:
            return False

        # Update agent status
        agent = self.agents[agent_id]
        agent.status.is_active = False

        # Notify agent to shutdown
        await self.message_bus.publish(
            f"agent.{agent_id}",
            Message(
                type="shutdown",
                sender=UUID(int=0),  # System ID
                receiver=agent_id,
                content={},
            ),
        )

        # Remove agent
        del self.agents[agent_id]
        logger.info(f"Terminated agent {agent_id}")
        return True

    async def get_agents_by_capability(self, capability: str) -> List[Agent]:
        """Find agents with specific capability."""
        return [
            agent
            for agent in self.agents.values()
            if any(cap.name == capability for cap in agent.config.capabilities)
        ]

    async def start(self):
        """Start agent manager background tasks."""
        asyncio.create_task(self._heartbeat_monitor())
        logger.info("Agent manager started")

    async def _heartbeat_monitor(self):
        """Monitor agent heartbeats and cleanup inactive agents."""
        while True:
            try:
                current_time = asyncio.get_event_loop().time()

                # Check each agent's last heartbeat
                for agent_id, agent in list(self.agents.items()):
                    if agent.status.last_heartbeat:
                        time_since_heartbeat = (
                            current_time - agent.status.last_heartbeat
                        )

                        # Terminate agent if heartbeat timeout
                        if time_since_heartbeat > self._heartbeat_interval * 2:
                            logger.warning(
                                f"Agent {agent_id} heartbeat timeout, terminating"
                            )
                            await self.terminate_agent(agent_id)

            except Exception as e:
                logger.error(f"Error in heartbeat monitor: {e}")

            await asyncio.sleep(self._heartbeat_interval)

    async def _handle_agent_message(self, message: Message):
        """Handle messages from agents."""
        try:
            if message.type == "heartbeat" and message.sender in self.agents:
                self.agents[message.sender].status.last_heartbeat = (
                    asyncio.get_event_loop().time()
                )

            elif message.type == "error" and message.sender in self.agents:
                self.agents[message.sender].status.error_count += 1
                logger.error(f"Agent {message.sender} error: {message.content}")

        except Exception as e:
            logger.error(f"Error handling agent message: {e}")
