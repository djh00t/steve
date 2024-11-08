# ai_agent/agents/analysis/analysis_agent.py

"""
Analysis agent implementation for data processing and insights.
"""
from typing import Dict, Any, List, Optional
from uuid import UUID, uuid4
from datetime import datetime, timedelta
import logging
import asyncio
import statistics
from collections import defaultdict

from pydantic import BaseModel, Field

from ..base import BaseAgent
from ...core.message_bus import Message

logger = logging.getLogger(__name__)


class AnalysisMetrics(BaseModel):
    """Metrics for analysis results."""

    count: int = 0
    mean: Optional[float] = None
    median: Optional[float] = None
    std_dev: Optional[float] = None
    min_value: Optional[float] = None
    max_value: Optional[float] = None
    percentiles: Dict[int, float] = Field(default_factory=dict)


class AnalysisResult(BaseModel):
    """Analysis result with metrics and insights."""

    metrics: AnalysisMetrics
    trends: List[Dict[str, Any]] = Field(default_factory=list)
    anomalies: List[Dict[str, Any]] = Field(default_factory=list)
    insights: List[str] = Field(default_factory=list)
    metadata: Dict[str, Any] = Field(default_factory=dict)
    timestamp: datetime = Field(default_factory=datetime.utcnow)


class AnalysisSession(BaseModel):
    """Active analysis session."""

    id: UUID = Field(default_factory=uuid4)
    data: Dict[str, Any]
    parameters: Dict[str, Any]
    results: List[AnalysisResult] = Field(default_factory=list)
    status: str = "active"
    created_at: datetime = Field(default_factory=datetime.utcnow)
    updated_at: datetime = Field(default_factory=datetime.utcnow)


class AnalysisAgent(BaseAgent):
    """Agent for data analysis and insight generation."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.active_sessions: Dict[UUID, AnalysisSession] = {}

    async def execute_task(self, task: Dict[str, Any]) -> Dict[str, Any]:
        """
        Execute an analysis task.

        Args:
            task: Task definition containing:
                - action: Analysis action to perform
                - data: Data to analyze
                - parameters: Analysis parameters
                - session_id: Optional session ID for continued analysis

        Returns:
            Dict containing analysis results
        """
        action = task.get("action")
        data = task.get("data", {})
        parameters = task.get("parameters", {})
        session_id = task.get("session_id")

        try:
            if action == "analyze_performance":
                return await self._analyze_performance(data, parameters)
            elif action == "analyze_resources":
                return await self._analyze_resources(data, parameters)
            elif action == "analyze_trends":
                return await self._analyze_trends(data, parameters)
            elif action == "generate_insights":
                return await self._generate_insights(session_id)
            else:
                raise ValueError(f"Unknown analysis action: {action}")

        except Exception as e:
            logger.error(f"Analysis task failed: {str(e)}")
            return {"success": False, "error": str(e)}

    async def _analyze_performance(
        self, data: Dict[str, Any], parameters: Dict[str, Any]
    ) -> Dict[str, Any]:
        """Analyze performance metrics."""
        metrics = {}
        trends = []
        insights = []

        # Process task completion times
        if "task_times" in data:
            completion_times = data["task_times"]
            metrics["completion_time"] = await self._calculate_metrics(completion_times)

            # Identify performance trends
            trends.extend(
                await self._identify_trends(
                    completion_times,
                    "completion_time",
                    parameters.get("trend_threshold", 0.1),
                )
            )

            # Generate insights
            if metrics["completion_time"].mean:
                insights.append(
                    f"Average task completion time: "
                    f"{metrics['completion_time'].mean:.2f} hours"
                )

                if metrics["completion_time"].std_dev:
                    insights.append(
                        f"Task time variability: "
                        f"{metrics['completion_time'].std_dev:.2f} hours"
                    )

        # Process resource utilization
        if "resource_usage" in data:
            for resource, usage in data["resource_usage"].items():
                metrics[f"resource_{resource}"] = await self._calculate_metrics(usage)

                # Identify resource bottlenecks
                if metrics[f"resource_{resource}"].max_value and metrics[
                    f"resource_{resource}"
                ].max_value > parameters.get("bottleneck_threshold", 0.9):
                    insights.append(
                        f"Potential bottleneck detected in {resource} "
                        f"(max utilization: "
                        f"{metrics[f'resource_{resource}'].max_value:.1%})"
                    )

        return {
            "success": True,
            "metrics": {k: v.dict() for k, v in metrics.items()},
            "trends": trends,
            "insights": insights,
        }

    async def _analyze_resources(
        self, data: Dict[str, Any], parameters: Dict[str, Any]
    ) -> Dict[str, Any]:
        """Analyze resource allocation and utilization."""
        resource_metrics = defaultdict(lambda: defaultdict(list))
        bottlenecks = []
        recommendations = []

        # Analyze resource allocation
        for resource, allocations in data.get("allocations", {}).items():
            # Calculate utilization over time
            utilization = await self._calculate_utilization(
                allocations, parameters.get("time_window", timedelta(hours=24))
            )

            resource_metrics[resource]["utilization"] = utilization

            # Check for overallocation
            peak_utilization = max(utilization.values())
            if peak_utilization > parameters.get("overallocation_threshold", 0.9):
                bottlenecks.append(
                    {
                        "resource": resource,
                        "peak_utilization": peak_utilization,
                        "timestamp": max(
                            utilization.keys(), key=lambda x: utilization[x]
                        ),
                    }
                )

                recommendations.append(
                    f"Consider increasing {resource} capacity or "
                    f"redistributing workload"
                )

        # Analyze resource conflicts
        conflicts = await self._analyze_resource_conflicts(
            data.get("allocations", {}), parameters.get("conflict_threshold", 0.8)
        )

        if conflicts:
            recommendations.extend(
                [
                    f"Resolve resource conflict between tasks "
                    f"{conflict['task1']} and {conflict['task2']} "
                    f"for resource {conflict['resource']}"
                    for conflict in conflicts
                ]
            )

        return {
            "success": True,
            "metrics": dict(resource_metrics),
            "bottlenecks": bottlenecks,
            "conflicts": conflicts,
            "recommendations": recommendations,
        }

    async def _analyze_trends(
        self, data: Dict[str, Any], parameters: Dict[str, Any]
    ) -> Dict[str, Any]:
        """Analyze trends in time series data."""
        trends = []
        seasonality = []
        anomalies = []

        for metric, time_series in data.items():
            # Sort by timestamp
            sorted_series = sorted(time_series.items(), key=lambda x: x[0])

            # Calculate moving averages
            window_size = parameters.get("window_size", 5)
            moving_avg = await self._calculate_moving_average(
                [v for _, v in sorted_series], window_size
            )

            # Detect trends
            trend = await self._detect_trend(moving_avg)
            if trend:
                trends.append(
                    {
                        "metric": metric,
                        "direction": trend["direction"],
                        "magnitude": trend["magnitude"],
                        "confidence": trend["confidence"],
                    }
                )

            # Detect seasonality
            season = await self._detect_seasonality(
                [v for _, v in sorted_series], parameters.get("seasonality_window", 24)
            )
            if season:
                seasonality.append(
                    {
                        "metric": metric,
                        "period": season["period"],
                        "strength": season["strength"],
                    }
                )

            # Detect anomalies
            detected_anomalies = await self._detect_anomalies(
                [v for _, v in sorted_series], parameters.get("anomaly_threshold", 2.0)
            )
            for anomaly in detected_anomalies:
                anomalies.append(
                    {
                        "metric": metric,
                        "timestamp": sorted_series[anomaly["index"]][0],
                        "value": sorted_series[anomaly["index"]][1],
                        "deviation": anomaly["deviation"],
                    }
                )

        return {
            "success": True,
            "trends": trends,
            "seasonality": seasonality,
            "anomalies": anomalies,
        }

    async def _generate_insights(self, session_id: UUID) -> Dict[str, Any]:
        """Generate insights from analysis results."""
        session = self.active_sessions.get(session_id)
        if not session:
            raise ValueError(f"Session {session_id} not found")

        insights = []
        recommendations = []

        # Analyze all results in session
        for result in session.results:
            # Performance insights
            if "completion_time" in result.metrics.dict():
                metrics = result.metrics.dict()["completion_time"]
                insights.extend(await self._generate_performance_insights(metrics))

            # Resource insights
            resource_metrics = {
                k: v
                for k, v in result.metrics.dict().items()
                if k.startswith("resource_")
            }
            if resource_metrics:
                resource_insights = await self._generate_resource_insights(
                    resource_metrics
                )
                insights.extend(resource_insights["insights"])
                recommendations.extend(resource_insights["recommendations"])

            # Trend insights
            if result.trends:
                insights.extend(await self._generate_trend_insights(result.trends))

            # Anomaly insights
            if result.anomalies:
                anomaly_insights = await self._generate_anomaly_insights(
                    result.anomalies
                )
                insights.extend(anomaly_insights["insights"])
                recommendations.extend(anomaly_insights["recommendations"])

        return {
            "success": True,
            "insights": insights,
            "recommendations": recommendations,
            "summary": await self._generate_summary(insights, recommendations),
        }

    async def _calculate_metrics(self, values: List[float]) -> AnalysisMetrics:
        """Calculate statistical metrics for a set of values."""
        if not values:
            return AnalysisMetrics(count=0)

        try:
            return AnalysisMetrics(
                count=len(values),
                mean=statistics.mean(values),
                median=statistics.median(values),
                std_dev=statistics.stdev(values) if len(values) > 1 else None,
                min_value=min(values),
                max_value=max(values),
                percentiles={
                    25: statistics.quantiles(values, n=4)[0],
                    50: statistics.quantiles(values, n=4)[1],
                    75: statistics.quantiles(values, n=4)[2],
                    90: statistics.quantiles(values, n=10)[8],
                },
            )
        except Exception as e:
            logger.error(f"Error calculating metrics: {str(e)}")
            return AnalysisMetrics(count=len(values))

    async def _calculate_moving_average(
        self, values: List[float], window_size: int
    ) -> List[float]:
        """Calculate moving average of values."""
        if not values or window_size <= 0:
            return []

        result = []
        for i in range(len(values)):
            if i < window_size - 1:
                result.append(sum(values[: i + 1]) / (i + 1))
            else:
                result.append(sum(values[i - window_size + 1 : i + 1]) / window_size)
        return result

    async def _detect_trend(self, values: List[float]) -> Optional[Dict[str, Any]]:
        """Detect trend in values."""
        if len(values) < 2:
            return None

        # Calculate simple linear regression
        n = len(values)
        x = list(range(n))
        x_mean = sum(x) / n
        y_mean = sum(values) / n

        # Calculate slope
        numerator = sum((xi - x_mean) * (yi - y_mean) for xi, yi in zip(x, values))
        denominator = sum((xi - x_mean) ** 2 for xi in x)

        if denominator == 0:
            return None

        slope = numerator / denominator

        # Calculate R-squared
        y_pred = [x_mean + slope * (xi - x_mean) for xi in x]
        ss_tot = sum((yi - y_mean) ** 2 for yi in values)
        ss_res = sum((yi - yp) ** 2 for yi, yp in zip(values, y_pred))

        r_squared = 1 - (ss_res / ss_tot) if ss_tot != 0 else 0

        return {
            "direction": "increasing" if slope > 0 else "decreasing",
            "magnitude": abs(slope),
            "confidence": r_squared,
        }

    async def _detect_seasonality(
        self, values: List[float], window: int
    ) -> Optional[Dict[str, Any]]:
        """Detect seasonality in values."""
        if len(values) < window * 2:
            return None

        # Calculate autocorrelation
        mean = sum(values) / len(values)
        normalized = [v - mean for v in values]

        autocorr = []
        for lag in range(1, min(window, len(values) // 2)):
            correlation = sum(
                normalized[i] * normalized[i + lag] for i in range(len(values) - lag)
            )
            autocorr.append(correlation)

        if not autocorr:
            return None

        # Find peaks in autocorrelation
        peaks = [
            i
            for i in range(1, len(autocorr) - 1)
            if autocorr[i] > autocorr[i - 1] and autocorr[i] > autocorr[i + 1]
        ]

        if not peaks:
            return None

        # Find strongest peak
        strongest_peak = max(peaks, key=lambda i: autocorr[i])

        return {"period": strongest_peak + 1, "strength": autocorr[strongest_peak]}

    async def _detect_anomalies(
        self, values: List[float], threshold: float
    ) -> List[Dict[str, Any]]:
        """Detect anomalies in values using Z-score."""
        if len(values) < 2:
            return []

        mean = statistics.mean(values)
        std_dev = statistics.stdev(values)

        if std_dev == 0:
            return []

        anomalies = []
        for i, value in enumerate(values):
            z_score = abs((value - mean) / std_dev)
            if z_score > threshold:
                anomalies.append({"index": i, "deviation": z_score})

        return anomalies

    async def _analyze_resource_conflicts(
        self, allocations: Dict[str, List[Dict[str, Any]]], threshold: float
    ) -> List[Dict[str, Any]]:
        """Analyze resource allocation conflicts."""
        conflicts = []

        for resource, allocs in allocations.items():
            # Sort allocations by time
            sorted_allocs = sorted(
                allocs, key=lambda x: (x["start_time"], x["end_time"])
            )

            # Check for overlaps
            for i in range(len(sorted_allocs)):
                for j in range(i + 1, len(sorted_allocs)):
                    if sorted_allocs[i]["end_time"] > sorted_allocs[j]["start_time"]:
                        # Calculate overlap
                        overlap_start = max(
                            sorted_allocs[i]["start_time"],
                            sorted_allocs[j]["start_time"],
                        )
                        overlap_end = min(
                            sorted_allocs[i]["end_time"], sorted_allocs[j]["end_time"]
                        )

                        total_allocation = (
                            sorted_allocs[i]["amount"] + sorted_allocs[j]["amount"]
                        )

                        if total_allocation > threshold:
                            conflicts.append(
                                {
                                    "resource": resource,
                                    "task1": sorted_allocs[i]["task_id"],
                                    "task2": sorted_allocs[j]["task_id"],
                                    "start_time": overlap_start,
                                    "end_time": overlap_end,
                                    "total_allocation": total_allocation,
                                }
                            )

        return conflicts
