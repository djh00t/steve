"""
Base agent implementation with common functionality.
Provides base classes for specialized agents.
"""

import asyncio
import logging
from abc import ABC, abstractmethod
from datetime import datetime
from typing import Any, Dict, List, Optional
from uuid import UUID, uuid4

from pydantic import BaseModel, Field

from ..core.message_bus import Message, MessageBus
from ..core.security_manager import SecurityContext, SecurityOperation

logger = logging.getLogger(__name__)


class AgentStatus(BaseModel):
    """Agent status information."""

    status: str = "idle"
    current_task: Optional[UUID] = None
    error_count: int = 0
    last_error: Optional[str] = None
    last_activity: datetime = Field(default_factory=datetime.utcnow)


class AgentMetrics(BaseModel):
    """Agent performance metrics."""

    tasks_completed: int = 0
    tasks_failed: int = 0
    average_task_duration: float = 0.0
    total_active_time: float = 0.0


class BaseAgent:
    """Base class for all agents."""

    def __init__(
        self, agent_id: UUID, message_bus: MessageBus, security_context: SecurityContext
    ):
        self.agent_id = agent_id
        self.message_bus = message_bus
        self.security_context = security_context
        self.status = AgentStatus()
        self._running = False
        self._message_handlers = self._setup_message_handlers()

    def _setup_message_handlers(self) -> Dict[str, callable]:
        """Set up message type handlers."""
        return {
            "task_assigned": self._handle_task_assigned,
            "task_cancelled": self._handle_task_cancelled,
            "shutdown": self._handle_shutdown,
            "status_request": self._handle_status_request,
        }

    async def execute_task(self, task: Dict[str, Any]) -> Dict[str, Any]:
        """
        Execute a task. Must be implemented by subclasses.

        Args:
            task: Task to execute

        Returns:
            Dict containing task results
        """
        raise NotImplementedError("Subclasses must implement execute_task")

    async def start(self):
        """Start the agent."""
        self._running = True
        await self.message_bus.subscribe(f"agent.{self.agent_id}", self._handle_message)
        logger.info(f"Agent {self.agent_id} started")

    async def stop(self):
        """Stop the agent."""
        self._running = False
        logger.info(f"Agent {self.agent_id} stopped")

    async def _handle_message(self, message: Message):
        """Handle incoming messages."""
        try:
            # Update last activity
            self.status.last_activity = datetime.utcnow()

            # Get message handler
            handler = self._message_handlers.get(message.type)
            if handler:
                await handler(message)
            else:
                logger.warning(f"Unhandled message type: {message.type}")

        except Exception as e:
            logger.error(f"Error handling message: {e}")
            self.status.error_count += 1
            self.status.last_error = str(e)

    async def _heartbeat_loop(self):
        """Send periodic heartbeat messages."""
        while self._running:
            try:
                await self.message_bus.publish(
                    "agent.heartbeat",
                    Message(
                        type="heartbeat",
                        sender=self.agent_id,
                        content={
                            "status": self.status.dict(),
                            "metrics": self.metrics.dict(),
                        },
                    ),
                )
            except Exception as e:
                logger.error(f"Error sending heartbeat: {e}")

            await asyncio.sleep(30)  # 30 second interval

    @abstractmethod
    async def execute_task(self, task: Dict[str, Any]) -> Dict[str, Any]:
        """
        Execute a task assigned to the agent.

        Args:
            task: Task definition and parameters

        Returns:
            Dict[str, Any]: Task execution results
        """
        pass

    async def _handle_task_assigned(self, message: Message):
        """Handle task assignment."""
        task_id = UUID(message.content["task_id"])
        self.status.status = "busy"
        self.status.current_task = task_id

        try:
            # Execute task
            start_time = datetime.utcnow()
            result = await self.execute_task(message.content)
            duration = (datetime.utcnow() - start_time).total_seconds()

            # Update metrics
            self.metrics.tasks_completed += 1
            self.metrics.average_task_duration = (
                self.metrics.average_task_duration * (self.metrics.tasks_completed - 1)
                + duration
            ) / self.metrics.tasks_completed
            self.metrics.total_active_time += duration

            # Send completion message
            await self.message_bus.publish(
                "task.completed",
                Message(
                    type="task_completed",
                    sender=self.agent_id,
                    content={
                        "task_id": str(task_id),
                        "result": result,
                        "success": True,
                    },
                ),
            )

        except Exception as e:
            logger.error(f"Error executing task {task_id}: {e}")
            self.status.error_count += 1
            self.status.last_error = str(e)
            self.metrics.tasks_failed += 1

            # Send failure message
            await self.message_bus.publish(
                "task.completed",
                Message(
                    type="task_completed",
                    sender=self.agent_id,
                    content={
                        "task_id": str(task_id),
                        "error": str(e),
                        "success": False,
                    },
                ),
            )

        finally:
            self.status.status = "idle"
            self.status.current_task = None

    async def _handle_task_cancelled(self, message: Message):
        """Handle task cancellation."""
        task_id = UUID(message.content["task_id"])
        if self.status.current_task == task_id:
            self.status.status = "idle"
            self.status.current_task = None
            logger.info(f"Task {task_id} cancelled")

    async def _handle_shutdown(self, message: Message):
        """Handle shutdown request."""
        await self.stop()

    async def _handle_status_request(self, message: Message):
        """Handle status request."""
        await self.message_bus.publish(
            message.content.get("reply_to", "agent.status"),
            Message(
                type="status_response",
                sender=self.agent_id,
                content={"status": self.status.dict(), "metrics": self.metrics.dict()},
            ),
        )

    async def validate_operation(
        self, operation: str, resource: str, required_permissions: List[str]
    ) -> bool:
        """
        Validate if an operation is allowed.

        Args:
            operation: Operation type
            resource: Resource being accessed
            required_permissions: Required permissions

        Returns:
            bool: True if operation is allowed
        """
        op = SecurityOperation(
            operation_type=operation,
            resource=resource,
            required_permissions=set(required_permissions),
        )

        return await self.security_context.validate_operation(
            self.security_context.context_id, op
        )
