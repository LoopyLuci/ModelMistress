#!/usr/bin/env bash
# =============================================================================
# Prometheus Serve - Kubernetes Deployment Script
# =============================================================================
# Usage:
#   ./deploy.sh [overlay]    Deploy with kustomize overlay
#   ./deploy.sh teardown     Remove all resources
#   ./deploy.sh status       Show deployment status
#   ./deploy.sh build        Build Docker images
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
K8S_DIR="$SCRIPT_DIR"
NAMESPACE="prometheus-serve"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log() { echo -e "${BLUE}[prometheus-serve]${NC} $*"; }
warn() { echo -e "${YELLOW}[prometheus-serve]${NC} $*"; }
err() { echo -e "${RED}[prometheus-serve]${NC} $*" >&2; }
ok() { echo -e "${GREEN}[prometheus-serve]${NC} $*"; }

# ---------- Build Docker images ----------
build_images() {
    log "Building CPU image..."
    docker build -t prometheus-serve:latest -f "$PROJECT_DIR/Dockerfile" "$PROJECT_DIR"
    ok "CPU image built: prometheus-serve:latest"

    log "Building ROCm image..."
    docker build --target rocm -t prometheus-serve:rocm-latest \
        --build-arg ROCM_VERSION=6.1.2 \
        -f "$PROJECT_DIR/Dockerfile" "$PROJECT_DIR"
    ok "ROCm image built: prometheus-serve:rocm-latest"
}

# ---------- Deploy ----------
deploy() {
    local overlay="${1:-}"

    log "Creating namespace..."
    kubectl apply -f "$K8S_DIR/namespace.yaml"

    if [ -n "$overlay" ]; then
        log "Deploying with overlay: $overlay"
        case "$overlay" in
            single-node-amd-gpu|single-node)
                kubectl apply -k "$K8S_DIR/overlays/"
                ;;
            *)
                err "Unknown overlay: $overlay"
                err "Available: single-node-amd-gpu"
                exit 1
                ;;
        esac
    else
        log "Deploying base configuration..."
        kubectl apply -k "$K8S_DIR/base/"
    fi

    ok "Deployment complete!"

    # Wait for GPU node discovery
    log "Waiting for GPU node discovery DaemonSet..."
    kubectl rollout status daemonset/prometheus-serve-gpu-discovery \
        -n "$NAMESPACE" --timeout=60s 2>/dev/null || warn "GPU discovery still starting"

    log "Waiting for deployments..."
    kubectl rollout status deployment/prometheus-serve-cpu \
        -n "$NAMESPACE" --timeout=120s 2>/dev/null || warn "CPU deployment still starting"
    kubectl rollout status deployment/prometheus-serve-gpu \
        -n "$NAMESPACE" --timeout=120s 2>/dev/null || warn "GPU deployment still starting"

    show_status
}

# ---------- Label AMD GPU node ----------
label_gpu_node() {
    local node_name="${1:-$(hostname)}"
    log "Labeling node $node_name for AMD GPU..."
    kubectl label node "$node_name" \
        prometheus-serve/node-type=gpu \
        amd.com/gpu="$(kubectl get node "$node_name" -o jsonpath='{.status.capacity.amd\.com/gpu}' 2>/dev/null || echo '1')" \
        amd.com/gpu.family=rdna3 \
        amd.com/gpu.gfx-version=11.0.0 \
        amd.com/gpu.vram-gb=24 \
        --overwrite
    ok "Node $node_name labeled for AMD GPU"
}

# ---------- Status ----------
show_status() {
    echo ""
    log "=== Deployment Status ==="
    kubectl get all -n "$NAMESPACE" -o wide
    echo ""
    log "=== GPU Node Labels ==="
    kubectl get nodes -l prometheus-serve/node-type=gpu --show-labels 2>/dev/null | head -5
    echo ""
    log "=== Pods ==="
    kubectl get pods -n "$NAMESPACE" -o wide
    echo ""
    log "=== Services ==="
    kubectl get svc -n "$NAMESPACE"
    echo ""
    log "=== HPA ==="
    kubectl get hpa -n "$NAMESPACE" 2>/dev/null
    echo ""
    log "=== DaemonSet ==="
    kubectl get ds -n "$NAMESPACE" 2>/dev/null
}

# ---------- Teardown ----------
teardown() {
    warn "Tearing down Prometheus Serve..."
    kubectl delete -k "$K8S_DIR/gpu-node-discovery/" --ignore-not-found 2>/dev/null || true
    kubectl delete -k "$K8S_DIR/base/" --ignore-not-found 2>/dev/null || true
    kubectl delete namespace "$NAMESPACE" --ignore-not-found 2>/dev/null || true
    ok "Teardown complete"
}

# ---------- Main ----------
case "${1:-help}" in
    build)
        build_images
        ;;
    deploy)
        deploy "${2:-}"
        ;;
    single-node)
        label_gpu_node "${2:-}"
        deploy single-node-amd-gpu
        ;;
    label-node)
        label_gpu_node "${2:-}"
        ;;
    status)
        show_status
        ;;
    teardown|destroy)
        teardown
        ;;
    *)
        echo "Usage: $0 {build|deploy [overlay]|single-node|label-node [node]|status|teardown}"
        echo ""
        echo "Commands:"
        echo "  build                    Build Docker images (CPU + ROCm)"
        echo "  deploy [overlay]         Deploy base or with overlay"
        echo "  single-node [node]       Label GPU node + deploy single-node overlay"
        echo "  label-node [node]        Label a node for AMD GPU scheduling"
        echo "  status                   Show deployment status"
        echo "  teardown                 Remove all resources"
        exit 1
        ;;
esac
