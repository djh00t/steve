# ai_agent/core/state_manager.py
"""
State management implementation.
Handles state storage, synchronization, and persistence.
"""
from typing import Dict, Any, Optional, List
import logging
import json
import asyncio

import redis.asyncio as redis
from pydantic import BaseModel

logger = logging.getLogger(__name__)

class StateEntry(BaseModel):
    """State entry with metadata."""
    key: str
    value: Any
    version: int
    temporary: bool = False
    metadata: Dict = {}

class StateManager:
    """Manages system state using Redis."""
    
    def __init__(self, redis_url: str):
        """Initialize state manager with Redis connection."""
        self.redis = redis.from_url(redis_url)
        self.local_cache: Dict[str, StateEntry] = {}
        self.version_counter = 0
        self._cache_enabled = True
        self._subscribers: Dict[str, List[callable]] = {}
        
    async def set_state(
        self,
        key: str,
        value: Any,
        temporary: bool = False,
        metadata: Optional[Dict] = None
    ) -> bool:
        """
        Set state value.
        
        Args:
            key: State key
            value: State value
            temporary: If True, state is not persisted
            metadata: Optional metadata
            
        Returns:
            bool: True if state was set
        """
        try:
            # Increment version
            self.version_counter += 1
            
            # Create state entry
            entry = StateEntry(
                key=key,
                value=value,
                version=self.version_counter,
                temporary=temporary,
                metadata=metadata or {}
            )
            
            # Update local cache
            if self._cache_enabled:
                self.local_cache[key] = entry
                
            # Persist if not temporary
            if not temporary:
                await self.redis.set(
                    f"state:{key}",
                    entry.json(),
                    ex=None if not temporary else 3600  # 1 hour for temporary
                )
                
            # Notify subscribers
            await self._notify_subscribers(key, entry)
            
            logger.debug(f"Set state {key} = {value} (version {self.version_counter})")
            return True
            
        except Exception as e:
            logger.error(f"Failed to set state {key}: {e}")
            return False
            
    async def get_state(
        self,
        key: str,
        default: Any = None
    ) -> Optional[Any]:
        """
        Get state value.
        
        Args:
            key: State key
            default: Default value if not found
            
        Returns:
            Optional[Any]: State value or default
        """
        try:
            # Check local cache first
            if self._cache_enabled and key in self.local_cache:
                return self.local_cache[key].value
                
            # Get from Redis
            value = await self.redis.get(f"state:{key}")
            if value:
                entry = StateEntry.parse_raw(value)
                
                # Update cache
                if self._cache_enabled:
                    self.local_cache[key] = entry
                    
                return entry.value
                
            return default
            
        except Exception as e:
            logger.error(f"Failed to get state {key}: {e}")
            return default
            
    async def delete_state(self, key: str) -> bool:
        """Delete state value."""
        try:
            # Remove from cache
            if key in self.local_cache:
                del self.local_cache[key]
                
            # Remove from Redis
            await self.redis.delete(f"state:{key}")
            
            # Notify subscribers
            await self._notify_subscribers(key, None)
            
            logger.debug(f"Deleted state {key}")
            return True
            
        except Exception as e:
            logger.error(f"Failed to delete state {key}: {e}")
            return False
            
    async def subscribe(self, key: str, callback: callable):
        """Subscribe to state changes."""
        if key not in self._subscribers:
            self._subscribers[key] = []
        self._subscribers[key].append(callback)
        
    async def unsubscribe(self, key: str, callback: callable):
        """Unsubscribe from state changes."""
        if key in self._subscribers:
            self._subscribers[key].remove(callback)
            
    async def _notify_subscribers(self, key: str, entry: Optional[StateEntry]):
        """Notify subscribers of state changes."""
        if key in self._subscribers:
            for callback in self._subscribers[key]:
                try:
                    await callback(key, entry.value if entry else None)
                except Exception as e:
                    logger.error(f"Error in state subscriber callback: {e}")
                    
    async def clear_cache(self):
        """Clear the local cache."""
        self.local_cache.clear()
        logger.debug("Cleared state cache")
        
    async def sync_cache(self):
        """Synchronize cache with Redis."""
        try:
            # Get all keys
            keys = await self.redis.keys("state:*")
            
            # Clear current cache
            self.local_cache.clear()
            
            # Rebuild cache
            for key in keys:
                value = await self.redis.get(key)
                if value:
                    entry = StateEntry.parse_raw(value)
                    stripped_key = key.decode().replace("state:", "")
                    self.local_cache[stripped_key] = entry
                    
            logger.debug(f"Synchronized cache with {len(keys)} entries")
            
        except Exception as e:
            logger.error(f"Failed to sync cache: {e}")
            
    async def get_all_keys(self) -> List[str]:
        """Get all state keys."""
        try:
            keys = await self.redis.keys("state:*")
            return [key.decode().replace("state:", "") for key in keys]
        except Exception as e:
            logger.error(f"Failed to get state keys: {e}")
            return []
            
    async def bulk_set_state(
        self,
        states: Dict[str, Any],
        temporary: bool = False,
        metadata: Optional[Dict] = None
    ) -> bool:
        """
        Set multiple state values atomically.
        
        Args:
            states: Dictionary of state key-value pairs
            temporary: If True, states are not persisted
            metadata: Optional metadata
            
        Returns:
            bool: True if all states were set
        """
        try:
            pipe = await self.redis.pipeline()
            
            for key, value in states.items():
                # Increment version
                self.version_counter += 1
                
                # Create state entry
                entry = StateEntry(
                    key=key,
                    value=value,
                    version=self.version_counter,
                    temporary=temporary,
                    metadata=metadata or {}
                )
                
                # Update local cache
                if self._cache_enabled:
                    self.local_cache[key] = entry
                    
                # Add to pipeline
                if not temporary:
                    await pipe.set(
                        f"state:{key}",
                        entry.json(),
                        ex=None if not temporary else 3600
                    )
                    
                # Notify subscribers
                await self._notify_subscribers(key, entry)
                
            # Execute pipeline
            await pipe.execute()
            
            logger.debug(f"Bulk set {len(states)} states")
            return True
            
        except Exception as e:
            logger.error(f"Failed to bulk set states: {e}")
            return False
            
    async def get_metadata(self, key: str) -> Optional[Dict]:
        """Get state entry metadata."""
        try:
            # Check cache first
            if self._cache_enabled and key in self.local_cache:
                return self.local_cache[key].metadata
                
            # Get from Redis
            value = await self.redis.get(f"state:{key}")
            if value:
                entry = StateEntry.parse_raw(value)
                return entry.metadata
                
            return None
            
        except Exception as e:
            logger.error(f"Failed to get state metadata for {key}: {e}")
            return None
            
    async def update_metadata(
        self,
        key: str,
        metadata: Dict
    ) -> bool:
        """Update state entry metadata."""
        try:
            # Get current entry
            value = await self.redis.get(f"state:{key}")
            if not value:
                return False
                
            entry = StateEntry.parse_raw(value)
            
            # Update metadata
            entry.metadata.update(metadata)
            
            # Save updated entry
            await self.redis.set(f"state:{key}", entry.json())
            
            # Update cache
            if self._cache_enabled:
                self.local_cache[key] = entry
                
            logger.debug(f"Updated metadata for state {key}")
            return True
            
        except Exception as e:
            logger.error(f"Failed to update state metadata for {key}: {e}")
            return False
            
    async def start(self):
        """Start state manager and sync cache."""
        await self.sync_cache()
        logger.info("State manager started")
        
    async def stop(self):
        """Stop state manager and clear cache."""
        await self.clear_cache()
        await self.redis.close()
        logger.info("State manager stopped")