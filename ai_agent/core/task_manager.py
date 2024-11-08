# ai_agent/core/task_manager.py
"""
Task management and distribution implementation.
Handles task creation, assignment, and lifecycle management.
"""
from typing import Dict, Optional, List
from uuid import UUID, uuid4
from datetime import datetime
import logging
import asyncio

from pydantic import BaseModel, Field

from .message_bus import MessageBus, Message
from .agent_manager import AgentManager

logger = logging.getLogger(__name__)


class TaskPriority(BaseModel):
    """Task priority configuration."""

    level: int = 0  # Higher number = higher priority
    deadline: Optional[datetime] = None


class TaskRequirements(BaseModel):
    """Requirements for task execution."""

    capabilities: List[str]
    min_memory_mb: int = 128
    min_cpu_cores: float = 0.1
    max_duration_seconds: int = 3600


class TaskResult(BaseModel):
    """Result of task execution."""

    success: bool
    data: Dict = Field(default_factory=dict)
    error: Optional[str] = None
    completed_at: datetime = Field(default_factory=datetime.utcnow)


class Task(BaseModel):
    """Task definition and state."""

    id: UUID = Field(default_factory=uuid4)
    type: str
    description: str
    priority: TaskPriority = Field(default_factory=TaskPriority)
    requirements: TaskRequirements
    status: str = "pending"
    agent_id: Optional[UUID] = None
    result: Optional[TaskResult] = None
    created_at: datetime = Field(default_factory=datetime.utcnow)
    started_at: Optional[datetime] = None
    parent_task: Optional[UUID] = None
    subtasks: List[UUID] = Field(default_factory=list)


class TaskManager:
    """Manages task lifecycle and distribution."""

    def __init__(self, message_bus: MessageBus, agent_manager: AgentManager):
        """Initialize task manager."""
        self.message_bus = message_bus
        self.agent_manager = agent_manager
        self.tasks: Dict[UUID, Task] = {}
        self.task_queue: List[UUID] = []

    async def create_task(
        self,
        task_type: str,
        description: str,
        requirements: TaskRequirements,
        priority: Optional[TaskPriority] = None,
        parent_task: Optional[UUID] = None,
    ) -> UUID:
        """
        Create a new task.

        Args:
            task_type: Type of task to create
            description: Task description
            requirements: Task requirements
            priority: Optional task priority
            parent_task: Optional parent task ID

        Returns:
            UUID: ID of created task
        """
        task = Task(
            type=task_type,
            description=description,
            requirements=requirements,
            priority=priority or TaskPriority(),
            parent_task=parent_task,
        )

        self.tasks[task.id] = task
        self.task_queue.append(task.id)

        # Sort queue by priority
        self.task_queue.sort(key=lambda x: self.tasks[x].priority.level, reverse=True)

        logger.info(f"Created task {task.id} of type {task_type}")
        return task.id

    async def get_task(self, task_id: UUID) -> Optional[Task]:
        """Get task by ID."""
        return self.tasks.get(task_id)

    async def assign_task(self, task_id: UUID, agent_id: UUID) -> bool:
        """
        Assign a task to an agent.

        Args:
            task_id: Task to assign
            agent_id: Agent to assign to

        Returns:
            bool: True if assignment successful
        """
        if task_id not in self.tasks or task_id not in self.task_queue:
            return False

        task = self.tasks[task_id]
        task.agent_id = agent_id
        task.status = "assigned"
        task.started_at = datetime.utcnow()

        # Remove from queue
        self.task_queue.remove(task_id)

        # Notify agent
        await self.message_bus.publish(
            f"agent.{agent_id}",
            Message(
                type="task_assigned",
                sender=UUID(int=0),  # System ID
                receiver=agent_id,
                content={"task_id": str(task_id)},
            ),
        )

        logger.info(f"Assigned task {task_id} to agent {agent_id}")
        return True

    async def complete_task(self, task_id: UUID, result: TaskResult) -> bool:
        """
        Mark a task as completed.

        Args:
            task_id: Task to complete
            result: Task execution result

        Returns:
            bool: True if completion successful
        """
        if task_id not in self.tasks:
            return False

        task = self.tasks[task_id]
        task.status = "completed" if result.success else "failed"
        task.result = result

        # Check parent task
        if task.parent_task and task.parent_task in self.tasks:
            parent = self.tasks[task.parent_task]
            if all(
                self.tasks[subtask_id].status == "completed"
                for subtask_id in parent.subtasks
            ):
                parent.status = "completed"

        logger.info(f"Task {task_id} completed with status: {task.status}")
        return True

    async def start(self):
        """Start task manager background tasks."""
        asyncio.create_task(self._task_scheduler())
        logger.info("Task manager started")

    async def _task_scheduler(self):
        """Background task scheduler."""
        while True:
            try:
                await self._process_task_queue()
            except Exception as e:
                logger.error(f"Error in task scheduler: {e}")
            await asyncio.sleep(1)  # Prevent tight loop

    async def _process_task_queue(self):
        """Process pending tasks in queue."""
        if not self.task_queue:
            return

        for task_id in self.task_queue[:]:  # Copy list for iteration
            task = self.tasks[task_id]

            # Find suitable agent
            suitable_agents = []
            for capability in task.requirements.capabilities:
                agents = await self.agent_manager.get_agents_by_capability(capability)
                suitable_agents.extend(agents)

            # Filter to agents that have all capabilities
            capable_agents = [
                agent
                for agent in suitable_agents
                if all(
                    cap in [c.name for c in agent.config.capabilities]
                    for cap in task.requirements.capabilities
                )
            ]

            # Filter to available agents
            available_agents = [
                agent
                for agent in capable_agents
                if len(agent.status.current_tasks) < agent.config.max_concurrent_tasks
            ]

            if available_agents:
                # Select agent (currently simple round-robin)
                selected_agent = available_agents[0]

                # Assign task
                success = await self.assign_task(task_id, selected_agent.id)
                if success:
                    selected_agent.status.current_tasks.append(task_id)

    async def create_subtasks(
        self, parent_id: UUID, subtasks: List[Task]
    ) -> List[UUID]:
        """
        Create subtasks for a parent task.

        Args:
            parent_id: Parent task ID
            subtasks: List of subtask definitions

        Returns:
            List[UUID]: List of created subtask IDs
        """
        if parent_id not in self.tasks:
            raise ValueError(f"Parent task {parent_id} not found")

        parent_task = self.tasks[parent_id]
        subtask_ids = []

        for subtask in subtasks:
            # Create subtask
            subtask_id = await self.create_task(
                task_type=subtask.type,
                description=subtask.description,
                requirements=subtask.requirements,
                priority=subtask.priority,
                parent_task=parent_id,
            )

            subtask_ids.append(subtask_id)
            parent_task.subtasks.append(subtask_id)

        return subtask_ids

    async def cancel_task(self, task_id: UUID) -> bool:
        """
        Cancel a task and its subtasks.

        Args:
            task_id: Task to cancel

        Returns:
            bool: True if cancellation successful
        """
        if task_id not in self.tasks:
            return False

        task = self.tasks[task_id]

        # Cancel subtasks first
        for subtask_id in task.subtasks:
            await self.cancel_task(subtask_id)

        # Remove from queue if pending
        if task_id in self.task_queue:
            self.task_queue.remove(task_id)

        # Notify agent if assigned
        if task.agent_id:
            await self.message_bus.publish(
                f"agent.{task.agent_id}",
                Message(
                    type="task_cancelled",
                    sender=UUID(int=0),  # System ID
                    receiver=task.agent_id,
                    content={"task_id": str(task_id)},
                ),
            )

        task.status = "cancelled"
        logger.info(f"Cancelled task {task_id}")
        return True

    async def get_task_status(self, task_id: UUID) -> Optional[str]:
        """Get current status of a task."""
        task = await self.get_task(task_id)
        return task.status if task else None

    async def get_task_result(self, task_id: UUID) -> Optional[TaskResult]:
        """Get result of a completed task."""
        task = await self.get_task(task_id)
        return task.result if task else None
