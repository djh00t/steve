"""
Security management and access control implementation.
Handles permissions, security contexts, and access validation.
"""
from typing import Dict, Set, Optional, List
from uuid import UUID, uuid4
import logging
from datetime import datetime

from pydantic import BaseModel, Field, ConfigDict

logger = logging.getLogger(__name__)

class SecurityOperation(BaseModel):
    """Security operation request."""
    operation_type: str
    resource: str
    required_permissions: Set[str]
    required_auth_level: int = 0

class Permission(BaseModel):
    """Permission definition."""
    name: str
    description: str
    required_auth_level: int = 0  # Higher = more restricted

class SecurityOperation(BaseModel):
    """Security operation request."""
    operation_type: str
    resource: str
    required_permissions: Set[str]
    required_auth_level: int = 0

class SecurityContext(BaseModel):
    """Security context for operations."""
    context_id: UUID = Field(default_factory=uuid4)
    model_config = ConfigDict(arbitrary_types_allowed=True)
    agent_id: UUID
    permissions: Set[str]
    auth_level: int
    created_at: datetime = Field(default_factory=datetime.utcnow)
    expires_at: Optional[datetime] = None
    metadata: Dict = Field(default_factory=dict)
    
    async def validate_operation(self, context_id: UUID, operation: SecurityOperation) -> bool:
        """
        Validate if an operation is allowed in this context.
        
        Args:
            context_id: The security context ID
            operation: Operation to validate
            
        Returns:
            bool: True if operation is allowed
        """
        # Verify context ID matches
        if context_id != self.context_id:
            return False
            
        # Check required permissions
        if not operation.required_permissions.issubset(self.permissions):
            return False
            
        # Check auth level
        if self.auth_level < operation.required_auth_level:
            return False
            
        # Check expiration
        if self.expires_at and self.expires_at < datetime.utcnow():
            return False
            
        return True

class AuditLog(BaseModel):
    """Audit log entry."""
    timestamp: datetime = Field(default_factory=datetime.utcnow)
    context_id: UUID
    agent_id: UUID
    operation: str
    resource: str
    success: bool
    error: Optional[str] = None

class SecurityManager:
    """Manages security and access control."""
    
    def __init__(self):
        """Initialize security manager."""
        self.permissions: Dict[str, Permission] = {}
        self.contexts: Dict[UUID, SecurityContext] = {}
        self.audit_logs: List[AuditLog] = []
        
    async def register_permission(self, permission: Permission):
        """Register a new permission type."""
        self.permissions[permission.name] = permission
        logger.info(f"Registered permission: {permission.name}")
        
    async def create_context(
        self,
        agent_id: UUID,
        permissions: Set[str],
        auth_level: int = 0,
        expires_at: Optional[datetime] = None
    ) -> SecurityContext:
        """
        Create a security context for an agent.
        
        Args:
            agent_id: Agent ID
            permissions: Set of permission names
            auth_level: Authorization level
            expires_at: Optional expiration time
            
        Returns:
            SecurityContext: Created security context
        """
        # Validate permissions
        invalid_permissions = permissions - self.permissions.keys()
        if invalid_permissions:
            raise ValueError(f"Invalid permissions: {invalid_permissions}")
            
        context = SecurityContext(
            agent_id=agent_id,
            permissions=permissions,
            auth_level=auth_level,
            expires_at=expires_at
        )
        
        self.contexts[context.context_id] = context
        logger.info(
            f"Created security context {context.context_id} for agent {agent_id}"
        )
        return context
        
    async def validate_operation(
        self,
        context_id: UUID,
        operation: SecurityOperation
    ) -> bool:
        """
        Validate if an operation is allowed in the given context.
        
        Args:
            context_id: Security context ID
            operation: Operation to validate
            
        Returns:
            bool: True if operation is allowed
        """
        context = self.contexts.get(context_id)
        if not context:
            await self._audit_log(
                context_id,
                UUID(int=0),
                operation.operation_type,
                operation.resource,
                False,
                "Invalid security context"
            )
            return False
            
        # Check context expiration
        if context.expires_at and context.expires_at < datetime.utcnow():
            await self._audit_log(
                context_id,
                context.agent_id,
                operation.operation_type,
                operation.resource,
                False,
                "Security context expired"
            )
            return False
            
        # Verify required permissions
        missing_permissions = operation.required_permissions - context.permissions
        if missing_permissions:
            await self._audit_log(
                context_id,
                context.agent_id,
                operation.operation_type,
                operation.resource,
                False,
                f"Missing permissions: {missing_permissions}"
            )
            return False
            
        # Verify auth level
        if context.auth_level < operation.required_auth_level:
            await self._audit_log(
                context_id,
                context.agent_id,
                operation.operation_type,
                operation.resource,
                False,
                "Insufficient auth level"
            )
            return False
            
        # Operation allowed
        await self._audit_log(
            context_id,
            context.agent_id,
            operation.operation_type,
            operation.resource,
            True
        )
        return True
        
    async def _audit_log(
        self,
        context_id: UUID,
        agent_id: UUID,
        operation: str,
        resource: str,
        success: bool,
        error: Optional[str] = None
    ):
        """Create audit log entry."""
        log = AuditLog(
            context_id=context_id,
            agent_id=agent_id,
            operation=operation,
            resource=resource,
            success=success,
            error=error
        )
        self.audit_logs.append(log)
        
    async def revoke_context(self, context_id: UUID):
        """Revoke a security context."""
        if context_id in self.contexts:
            del self.contexts[context_id]
            logger.info(f"Revoked security context {context_id}")
            
    async def get_audit_logs(
        self,
        agent_id: Optional[UUID] = None,
        operation: Optional[str] = None,
        start_time: Optional[datetime] = None,
        end_time: Optional[datetime] = None
    ) -> List[AuditLog]:
        """Get filtered audit logs."""
        filtered_logs = self.audit_logs
        
        if agent_id:
            filtered_logs = [log for log in filtered_logs if log.agent_id == agent_id]
            
        if operation:
            filtered_logs = [log for log in filtered_logs if log.operation == operation]
            
        if start_time:
            filtered_logs = [log for log in filtered_logs if log.timestamp >= start_time]
            
        if end_time:
            filtered_logs = [log for log in filtered_logs if log.timestamp <= end_time]
            
        return filtered_logs
