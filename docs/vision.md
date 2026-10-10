# Image recognition

ModelMistress recognises pictures with ONNX Runtime built into the binary (feature `vision`, off by default so the
default build stays small): it can **classify** a picture, find the **objects** in it, or turn it into an **embedding**
vector. It is the `ort` crate (2.0.0-rc.13) for the models and the `image` crate (PNG and JPEG only) for decoding. Models
run on the CPU.

## Building

```bash
cargo build --release -p model-mistress --features vision
```

`ort`'s build downloads (or finds in its cache) the ONNX Runtime library for the platform and links it in. Building
offline needs that cache filled beforehand. Without the feature the three routes and the
operation answer `501` and say how to rebuild, and `model.list` does not offer vision models.

## Models

Put `.onnx` files in `<home>/vision/` (or in `<home>/models/` or a folder in `model_dirs`). The id is the file name
without `.onnx`; with no `model` in a request, the only vision model found is used. Models are never downloaded for you.
Only float32 models with one colour-picture input (`[1, 3, height, width]`, or channels last `[1, height, width, 3]`)
are supported; the batch is always 1.

What a model does is read from its outputs, unless a sidecar says:

| Output | Kind |
|---|---|
| a name containing `embed` or `pooler` | **embed** (CLIP's `image_embeds`, a pooled output) |
| `[1, n]` (or `[1, n, 1, 1]`) with `n` = 1000 or 1001, or with `labels` in the sidecar | **classify** |
| any other `[1, n]` | **embed** |
| `[1, a, b]`, YOLO-style: `[1, 4 + classes, boxes]` (v8) or `[1, boxes, 5 + classes]` (v5, with objectness) | **detect** |
| `[1, tokens, dim]` named `*hidden*` | **embed** (the first token) |

Anything else needs `"kind"` in the sidecar. Box coordinates must be in input pixels, as centre and size, which is what
the Ultralytics exports without built-in NMS give; the NMS is done here.

### The sidecar

An optional `<name>.json` beside the model (`resnet50.onnx` has `resnet50.json`; `resnet50.onnx.json` also works).
Every field is optional:

```json
{
  "kind": "classify",
  "labels": ["tench", "goldfish", "..."],
  "mean": [0.485, 0.456, 0.406], "std": [0.229, 0.224, 0.225], "scale": 0.00392156862,
  "size": [224, 224], "resize": "crop", "bgr": false,
  "objectness": false, "nms_iou": 0.45, "normalize": true
}
```

- `kind`: `classify`, `detect` or `embed`. `labels`: the class names in the model's order (a missing one is
  `class N`; a detector with 80 classes and no labels gets the COCO names).
- `mean`, `std` (per channel R, G, B) and `scale` (multiplies the 0..255 pixels first; default 1/255, use 1 for models
  that want 0..255). Defaults by kind: classifiers ImageNet's mean and std, embedders CLIP's, detectors none (0..1).
- `size`: the input size, `224` or `[width, height]`; only for models whose input height or width is dynamic (a fixed
  input size wins). Without it: 224, and 640 for detectors.
- `resize`: `crop` (the short side is scaled to the size and the centre is cut; the default for classifiers and
  embedders) or `stretch` (the whole picture is squeezed; the default for detectors, which means no letterbox: boxes are
  mapped back to the picture, but a model trained with letterboxing is a little less accurate). Scaling is bilinear, after
  a block average when shrinking by 2 or more.
- `bgr`: the model wants blue, green, red. `objectness`: force the YOLO layout (v5 yes, v8 no). `nms_iou`: the overlap
  above which two boxes of one class are one object (default 0.45). `normalize`: scale an embedding to length 1 (default
  yes; cosine similarity is then a dot product).

## The API

`POST /v1/vision/classify`, `/v1/vision/detect` and `/v1/vision/embed` take JSON: `model` (optional), `image` (a PNG or
JPEG as base64 or a `data:image/...;base64,` URL; up to 48 MB), and optionally `top_k` (1 to 1000: predictions, default
5; detections, default 100) and `threshold` (0 to 1: the least score of a box, default 0.25, or of a prediction, default
none). A model of another kind is refused with `400` and the name of the right route. The answers:

```json
{"kind": "classify", "model": "resnet50", "predictions": [{"class": 281, "label": "tabby cat", "score": 0.8731}]}
{"kind": "detect", "model": "yolov8n", "image": {"width": 640, "height": 480},
 "detections": [{"class": 16, "label": "dog", "score": 0.91, "box": {"x1": 12, "y1": 40, "x2": 300, "y2": 470}}]}
{"kind": "embed", "model": "clip-vision", "dimensions": 512, "embedding": [0.012, -0.044, "..."]}
```

Scores are probabilities (a classifier's output is softmaxed unless it already sums to 1); boxes are corners in the
picture's own pixels.

```bash
curl -s http://127.0.0.1:PORT/v1/vision/classify -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d "{\"model\": \"resnet50\", \"image\": \"$(base64 -w0 cat.jpg)\", \"top_k\": 3}"
```

A peer key may use the three routes (the key's model list applies to the model name; an empty `model` counts as
`vision`). Errors: `400` a bad request or an image that cannot be read, `404` no such model or none at all, `501` built
without the feature, `500` the model could not be loaded or has an unsupported shape (the message says why). One loaded
model is kept between requests (reloaded when its file changes); requests for it run one after another.

## Operation, listing and CLI

The operation `vision.analyze` takes the same fields, with `file` (a path on the hub's machine) instead of `image`
allowed, and answers by whatever the model is. `model.list` lists the models in the vision folders with `"kind":
"vision"` (and `"task"`, the sidecar's `kind` or null). The command reads the file for you:

```bash
model-mistress vision resnet50 cat.jpg --top-k 3
model-mistress vision yolov8n street.png --threshold 0.4
model-mistress vision clip-vision photo.jpg --json
```

## Models to try

Any of these export to the supported shape: torchvision or timm classifiers exported to ONNX (ResNet, MobileNet,
EfficientNet; add a sidecar with the 1000 ImageNet labels), the ONNX Model Zoo's classifiers, Ultralytics YOLOv5/v8/11
detectors exported with `format=onnx` (labels are COCO's by default), and the vision tower of CLIP (for example the
`vision_model.onnx` that Hugging Face Optimum exports), whose output is `image_embeds`.

## Tests

Request and sidecar parsing, model lookup, preprocessing (NCHW and NHWC, crop and stretch, normalisation), softmax,
top-k, overlap and NMS, YOLO decoding, kind guessing and the answer building are unit tests that need no model and run
in every build. With `--features vision` a model written by hand as protobuf bytes in the test (a global average
pool, flatten and softmax over the picture, so its three classes are the share of red, green and blue) goes through the
real ONNX Runtime session, the decoder and the hub routes. Real-world models are not part of the tests.
