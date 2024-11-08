# Use Ubuntu latest as base
FROM ubuntu:latest

# Avoid prompts from apt
ENV DEBIAN_FRONTEND=noninteractive

# Install essential packages including Chrome and X11
RUN apt-get update && apt-get install -y \
    python3.12 \
    python3-pip \
    python3.12-venv \
    chromium-browser \
    chromium-chromedriver \
    xvfb \
    xauth \
    curl \
    wget \
    unzip \
    fonts-liberation \
    libnss3 \
    libfontconfig1 \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd -m -s /bin/bash agent

# Set up Python environment
ENV VIRTUAL_ENV=/home/agent/venv
RUN python3.12 -m venv $VIRTUAL_ENV
ENV PATH="$VIRTUAL_ENV/bin:$PATH"

# Install Python dependencies
COPY requirements.txt /tmp/
RUN pip install --no-cache-dir -r /tmp/requirements.txt

# Set up Xvfb
ENV DISPLAY=:99

# Set up working directory
WORKDIR /home/agent/workspace

# Copy agent code
COPY --chown=agent:agent . /home/agent/app/

# Switch to non-root user
USER agent

# Start Xvfb and the agent
CMD ["bash", "-c", "Xvfb :99 -screen 0 1920x1080x24 -ac +extension GLX +render -noreset & python -m agent.main"]