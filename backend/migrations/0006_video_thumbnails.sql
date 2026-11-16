-- Re-index existing videos once so installations that upgrade gain
-- ffmpeg-generated gallery thumbnails. New uploads are queued on insert; this
-- only touches video nodes that were already indexed without a thumbnail.
UPDATE nodes
SET index_status = 'pending',
    index_error = NULL,
    index_attempts = 0
WHERE kind = 'file'
  AND mime LIKE 'video/%'
  AND thumb_sha IS NULL
  AND index_status <> 'pending';
