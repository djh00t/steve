# ai_agent/core/message_bus.py
"""
Message bus implementation for agent communication.
Implements a Redis-based message bus for reliable agent communication.
"""
from typing import Dict, Any, Callable, Optional
from uuid import UUID, uuid4
from datetime import datetime
import asyncio
import json
import logging

import redis.asyncio as redis
from pydantic import BaseModel, Field, ConfigDict
from typing import Any

logger = logging.getLogger(__name__)


class Message(BaseModel):
    """Message structure for inter-agent communication."""

    id: UUID = Field(default_factory=uuid4)
    type: str
    sender: UUID
    receiver: Optional[UUID] = None
    content: Dict[str, Any]
    timestamp: datetime = Field(default_factory=datetime.utcnow)
    reply_to: Optional[UUID] = None

    model_config = ConfigDict(arbitrary_types_allowed=True)

    def model_dump(self, **kwargs: Any) -> Dict[str, Any]:
        data = super().model_dump(**kwargs)
        # Serialize datetime
        if self.timestamp:
            data["timestamp"] = self.timestamp.isoformat()
        # Serialize UUIDs
        for field in ["id", "sender", "receiver", "reply_to"]:
            if data.get(field):
                data[field] = str(data[field])
        return data


class MessageBus:
    """Redis-based message bus implementation."""

    def __init__(self, redis_url: str):
        """Initialize message bus with Redis connection."""
        self.redis = redis.from_url(redis_url)
        self.subscribers: Dict[str, list[Callable]] = {}
        self._running = False

    async def publish(self, channel: str, message: Message) -> bool:
        """
        Publish a message to a channel.

        Args:
            channel: Channel to publish to
            message: Message to publish

        Returns:
            bool: True if publish successful
        """
        try:
            await self.redis.publish(channel, message.json())
            logger.debug(f"Published message {message.id} to channel {channel}")
            return True
        except Exception as e:
            logger.error(f"Failed to publish message: {e}")
            return False

    async def subscribe(self, channel: str, callback: Callable[[Message], Any]):
        """
        Subscribe to a channel with callback.

        Args:
            channel: Channel to subscribe to
            callback: Async callback function for messages
        """
        if channel not in self.subscribers:
            self.subscribers[channel] = []
        self.subscribers[channel].append(callback)
        logger.debug(f"Added subscriber to channel {channel}")

    async def unsubscribe(self, channel: str, callback: Callable[[Message], Any]):
        """
        Unsubscribe callback from channel.

        Args:
            channel: Channel to unsubscribe from
            callback: Callback to remove
        """
        if channel in self.subscribers:
            self.subscribers[channel].remove(callback)
            logger.debug(f"Removed subscriber from channel {channel}")

    async def start(self):
        """Start the message bus subscription handler."""
        self._running = True
        pubsub = self.redis.pubsub()

        # Subscribe to all channels with subscribers
        channels = list(self.subscribers.keys())
        if channels:
            await pubsub.subscribe(*channels)
            logger.info(f"Subscribed to channels: {channels}")

        # Start message handling loop
        while self._running:
            try:
                message = await pubsub.get_message(timeout=1.0)
                if message and message["type"] == "message":
                    await self._handle_message(message)
            except Exception as e:
                logger.error(f"Error handling message: {e}")
                await asyncio.sleep(1)  # Prevent tight loop on errors

    async def _handle_message(self, raw_message: Dict[str, Any]):
        """Handle incoming Redis message."""
        try:
            channel = raw_message["channel"].decode()
            message = Message.parse_raw(raw_message["data"])

            # Call all subscribers for this channel
            for callback in self.subscribers.get(channel, []):
                try:
                    await callback(message)
                except Exception as e:
                    logger.error(f"Error in subscriber callback: {e}")

        except Exception as e:
            logger.error(f"Error parsing message: {e}")

    async def stop(self):
        """Stop the message bus."""
        self._running = False
        await self.redis.close()
        logger.info("Message bus stopped")
