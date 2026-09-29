# Model Mistress - Kubernetes Deployment Guide

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                        model-mistress namespace                        │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  ┌──────────────────────────────────────────────────────────────────┐   │
│  │                    Services (Load Balancing)                      │   │
│  │                                                                   │   │
│  │  model-mistress (all)  ← ClusterIP :8000                       │   │
│  │  model-mistress-cpu    ← ClusterIP :8000 (CPU pods only)       │   │
│  │  model-mistress-gpu    ← ClusterIP :8000 (GPU pods only)       │   │
│  │  model-mistress-headless ← None (DNS discovery)                │   │
│  │  model-mistress-external ← NodePort :30080                     │   │
│  └──────────────┬───────────────────────────┬───────────────────────┘   │
│                 │                           │                            │
│  ┌──────────────▼───────┐  ┌────────────────▼──────────────────────┐   │
│  │   CPU Workers (2+)   │  │   GPU Workers (1-4, AMD ROCm)         │   │
│  │   2-8 replicas       │  │   1-4 replicas                        │   │
│  │   2-8 CPU, 4-16Gi    │  │   4-12 CPU, 12-48Gi RAM, 1 GPU       │   │
│  │   No GPU affinity    │  │   AMD RX 7900 XTX (24GB VRAM)         │   │
│  └──────────────────────┘  └───────────────────────────────────────┘   │
│                                                                          │
│  ┌──────────────────────────────────────────────────────────────────┐   │
│  │              GPU Node Discovery DaemonSet                         │   │
│  │  Runs on every AMD GPU node: discovers GPUs, labels nodes,       │   │
│  │  exports gpu_utilization metrics for HPA scaling                  │   │
│  └──────────────────────────────────────────────────────────────────┘   │
│                                                                          │
│  ┌──────────────────────────────────────────────────────────────────┐   │
│  │                    HPAs (Auto-scaling)                            │   │
│  │  GPU HPA: scales on GPU util >75%, RPS >50, VRAM >20GB          │   │
│  │  CPU HPA: scales on CPU >70%, memory >75%, RPS >100              │   │
│  └──────────────────────────────────────────────────────────────────┘   │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

## Quick Start (Single AMD GPU Node)

```bash
# 1. Build images
cd Z:\Projects\model-mistress
./k8s/deploy.sh build

# 2. Label your GPU node and deploy
./k8s/deploy.sh single-node

# 3. Verify
kubectl get pods -n model-mistress -o wide
kubectl logs -n model-mistress deployment/model-mistress-gpu -f

# 4. Test the API
kubectl port-forward -n model-mistress svc/model-mistress 8000:8000
curl http://localhost:8000/health
```

## File Structure

```
k8s/
├── namespace.yaml              # Dedicated namespace
├── configmap.yaml              # Hardware-specific settings (CPU/GPU variants)
├── deployment-cpu.yaml         # CPU worker deployment (no GPU affinity)
├── deployment-gpu.yaml         # GPU worker deployment (AMD ROCm, RDNA3 affinity)
├── services.yaml               # 5 service definitions (main, CPU, GPU, headless, external)
├── hpa.yaml                    # HorizontalPodAutoscalers (GPU util + CPU metrics)
├── pdb.yaml                    # PodDisruptionBudgets for both worker types
├── pvc.yaml                    # Persistent storage for models + cache
├── networkpolicy.yaml          # Network security policies
├── monitoring.yaml             # PodMonitor for Model Mistress
├── deploy.sh                   # Deployment/teardown script
├── README.md                   # This file
├── base/
│   └── kustomization.yaml      # Base Kustomize config (all standard resources)
├── gpu-node-discovery/
│   ├── daemonset.yaml          # GPU discovery DaemonSet + RBAC
│   └── kustomization.yaml      # Kustomize overlay with DaemonSet
└── overlays/
    └── single-node-amd-gpu.yaml # Single AMD GPU node overlay
```

## Key Configuration

### GPU Node Labels (required)
```bash
# Manual labeling
kubectl label node <node-name> model-mistress/node-type=gpu
kubectl label node <node-name> amd.com/gpu=1
kubectl label node <node-name> amd.com/gpu.family=rdna3
kubectl label node <node-name> amd.com/gpu.gfx-version=11.0.0
kubectl label node <node-name> amd.com/gpu.vram-gb=24

# Or use the DaemonSet (auto-labels after deploy)
./k8s/deploy.sh label-node <node-name>
```

### AMD ROCm Environment Variables
| Variable | Value | Description |
|----------|-------|-------------|
| `ROCM_PATH` | `/opt/rocm` | ROCm installation path |
| `HIP_VISIBLE_DEVICES` | `0` | GPU device index |
| `HSA_OVERRIDE_GFX_VERSION` | `11.0.0` | RDNA3 GFX version |
| `PYTORCH_HIP_ALLOC_CONF` | `expandable_segments:True` | Memory allocation |

### Resource Targets (RX 7900 XTX)
| Resource | GPU Worker | CPU Worker |
|----------|-----------|-----------|
| CPU request/limit | 4 / 12 cores | 2 / 8 cores |
| Memory request/limit | 12Gi / 48Gi | 4Gi / 16Gi |
| GPU | 1x AMD GPU | 0 |
| Max VRAM usage | 22GB (92%) | N/A |
| Batch size | 32 | 8 |
| Context length | 8192 | 4090 |

### HPA Scaling Thresholds
| Metric | GPU HPA | CPU HPA |
|--------|---------|---------|
| GPU utilization | >75% → scale up | N/A |
| CPU utilization | >80% (fallback) | >70% |
| Memory utilization | N/A | >75% |
| VRAM usage | >20GB | N/A |
| Requests/sec | >50/pod | >100/pod |
| Min replicas | 1 | 2 |
| Max replicas | 4 | 8 |
| Scale-up window | 120s | 60s |
| Scale-down window | 300s (5min) | 120s |

## Multi-node Cluster

For multi-node GPU clusters:

```bash
# Label all GPU nodes
for node in gpu-node-1 gpu-node-2 gpu-node-3; do
    kubectl label node $node model-mistress/node-type=gpu
    kubectl label node $node amd.com/gpu=1
    kubectl label node $node amd.com/gpu.family=rdna3
done

# Deploy base (will spread across labeled nodes)
kubectl apply -k k8s/base/
kubectl apply -k k8s/gpu-node-discovery/

# Increase GPU replicas to match available nodes
kubectl scale deployment model-mistress-gpu --replicas=3 -n model-mistress
```
