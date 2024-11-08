# tests/conftest.py

"""Test configuration and fixtures."""
import asyncio
from uuid import uuid4

import pytest

from ai_agent.core.message_bus import MessageBus
from ai_agent.core.security_manager import SecurityContext


@pytest.fixture
async def message_bus():
    """Create a message bus instance for testing."""
    bus = MessageBus("redis://localhost")
    await bus.start()
    yield bus
    await bus.stop()


@pytest.fixture
async def security_context():
    """Create a security context for testing."""
    return SecurityContext(
        agent_id=uuid4(), permissions={"execute_command", "web_access"}, auth_level=0
    )
