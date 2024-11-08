# ai_agent/main.py

"""Main entry point for the AI Agent system."""
import asyncio
import logging

import uvicorn

from ai_agent.api.routes import app
from ai_agent.core.agent_manager import AgentManager
from ai_agent.core.message_bus import MessageBus

logger = logging.getLogger(__name__)


async def startup():
    """Initialize system components."""
    # Initialize core components
    message_bus = MessageBus("redis://redis:6379")
    agent_manager = AgentManager(message_bus)

    # Start components
    await message_bus.start()
    await agent_manager.start()

    logger.info("AI Agent system initialized")


def main():
    """Main entry point."""
    # Set up logging
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s - %(name)s - %(levelname)s - %(message)s",
    )

    # Start the system
    asyncio.run(startup())

    # Run the API server
    uvicorn.run(app, host="0.0.0.0", port=8000)


if __name__ == "__main__":
    main()
