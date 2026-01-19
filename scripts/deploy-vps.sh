#!/bin/bash
# HIP-3 Bot VPS Deployment Script
# Usage: ./scripts/deploy-vps.sh <vps-host>

set -e

VPS_HOST="${1:-}"
VPS_USER="${2:-root}"
DEPLOY_DIR="/opt/hip3-bot"

if [ -z "$VPS_HOST" ]; then
    echo "Usage: $0 <vps-host> [vps-user]"
    echo "Example: $0 192.168.1.100 ubuntu"
    exit 1
fi

echo "=== HIP-3 Bot VPS Deployment ==="
echo "Host: $VPS_USER@$VPS_HOST"
echo "Deploy Dir: $DEPLOY_DIR"
echo ""

# Files to transfer
FILES=(
    "Cargo.toml"
    "Cargo.lock"
    "Dockerfile"
    "docker-compose.yml"
    ".dockerignore"
    "crates"
    "config"
)

# Create deployment archive
echo "Creating deployment archive..."
tar -czf /tmp/hip3-deploy.tar.gz "${FILES[@]}"

# Transfer to VPS
echo "Transferring to VPS..."
scp /tmp/hip3-deploy.tar.gz "$VPS_USER@$VPS_HOST:/tmp/"

# Deploy on VPS
echo "Deploying on VPS..."
ssh "$VPS_USER@$VPS_HOST" << 'ENDSSH'
set -e

DEPLOY_DIR="/opt/hip3-bot"

# Create deploy directory
sudo mkdir -p $DEPLOY_DIR
sudo chown $USER:$USER $DEPLOY_DIR

# Extract archive
cd $DEPLOY_DIR
tar -xzf /tmp/hip3-deploy.tar.gz
rm /tmp/hip3-deploy.tar.gz

# Create data directory
mkdir -p data/mainnet/signals

# Install Docker if not present
if ! command -v docker &> /dev/null; then
    echo "Installing Docker..."
    curl -fsSL https://get.docker.com | sudo sh
    sudo usermod -aG docker $USER
    echo "Docker installed. Please re-login and run this script again."
    exit 0
fi

# Build and start
echo "Building Docker image..."
docker compose build

echo "Starting container..."
docker compose up -d

echo ""
echo "=== Deployment Complete ==="
echo "Check status: docker compose logs -f"
echo "Stop: docker compose down"
ENDSSH

# Cleanup
rm /tmp/hip3-deploy.tar.gz

echo ""
echo "=== Deployment Complete ==="
echo "SSH to VPS and check: docker compose logs -f"
