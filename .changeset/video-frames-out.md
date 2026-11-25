**`author-from-video --frames-out <dir>` shows what the model would see.**
When a video draft missed a step there was no way to tell whether the model
misread a frame or never got one. `--frames-out` chooses the frames a draft
would be made from and writes them as JPEGs beside a `manifest.json` with each
frame's time, the part of the screen kept after trimming, and how many changed
moments the 60-frame budget dropped. No model is called and no draft is
written. It is also how the new video-to-flow evals read frame selection.
