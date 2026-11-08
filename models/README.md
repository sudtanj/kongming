Text detection and recognition models for the `ocrs` OCR engine (ONNX, loaded by rten), embedded by `src/doc.rs` to read scanned PDFs.
Downloaded from https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.onnx and .../text-recognition.onnx (project: https://github.com/robertknight/ocrs-models).
Upstream trains them only on open, non-restrictively licensed datasets, currently HierText (CC-BY-SA 4.0); it states no separate licence for the weights, so keep this attribution.
