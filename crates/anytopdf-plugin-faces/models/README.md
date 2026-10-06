# Embedded face detection model

`face_detection_yunet_2026may.onnx` is YuNet from the OpenCV model zoo
(<https://github.com/opencv/opencv_zoo/tree/main/models/face_detection_yunet>),
trained by Shiqi Yu and contributors and re-exported upstream with
symbolic height and width, under the MIT licence in `LICENSE-YUNET`.

- SHA-256: `ebafce4e3c118d6554634be5c27ab333b4c047a9a8c3faf1d7cf93101c22f0f0`
- Size: 229738 bytes

It is compiled into `anytopdf-plugin-faces` so face detection works without a
download. `ANYTOPDF_FACES_MODEL` replaces it with another YuNet-compatible
export.
