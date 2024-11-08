"""Research Agent main module."""

import asyncio
import logging
from ai_agent.core.message_bus import MessageBus

logger = logging.getLogger(__name__)


async def run_research_agent():
    """Initialize and run the research agent."""
    message_bus = MessageBus("redis://redis:6379")
    await message_bus.start()
    await message_bus.subscribe(["research_tasks"])

    logger.info("Research agent initialized and running")

    try:
        while True:
            await asyncio.sleep(1)
    except Exception as e:
        logger.error(f"Error in research agent: {e}")
    finally:
        await message_bus.stop()


def main():
    """Main entry point for research agent."""
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s - %(name)s - %(levelname)s - %(message)s",
    )

    asyncio.run(run_research_agent())


if __name__ == "__main__":
    main()
