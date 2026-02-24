#!/usr/bin/env bash
# Downloads all-MiniLM-L6-v2 ONNX model and tokenizer from HuggingFace
# for use with the Ethos ONNX embedding backend.
#
# Usage: ./scripts/download-onnx-model.sh

set -euo pipefail

MODEL_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/ethos/models"
mkdir -p "$MODEL_DIR"

BASE="https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main"

echo "Downloading ONNX model (~22MB)..."
curl --fail --show-error -L "$BASE/onnx/model.onnx" -o "$MODEL_DIR/all-MiniLM-L6-v2.onnx"

echo "Downloading tokenizer..."
curl --fail --show-error -L "$BASE/tokenizer.json" -o "$MODEL_DIR/all-MiniLM-L6-v2-tokenizer.json"

echo ""
echo "Done. Files saved to: $MODEL_DIR"
echo "  Model:     $MODEL_DIR/all-MiniLM-L6-v2.onnx"
echo "  Tokenizer: $MODEL_DIR/all-MiniLM-L6-v2-tokenizer.json"
