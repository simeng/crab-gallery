# File Watcher Implementation Notes

## Key Decision: Use notify::RecommendedWatcher, NOT Polling

**DO NOT poll every 500ms or any interval.** The user explicitly requested:
> "stop scanning everything every 0.5s, add a file watcher that only appends or removes the particular files that are added or removed from the folder while leaving the other files alone"

## Why Polling Was Wrong

1. **Inefficient**: Scans entire directory tree on each poll (every 500ms)
2. **Wasteful**: Rescans all existing images even though they haven't changed
3. **Memory pressure**: Loading same images repeatedly, especially with VipsImage

## Correct Approach: notify::RecommendedWatcher

The file watcher should:
1. Watch the `./images` directory recursively
2. Handle only CREATE/REMOVE events (add/remove files)
3. For MODIFY events, reload and update just that specific image
4. Leave all other images untouched - don't rescan them
5. Use async task spawning inside sync callback to handle VipsImage safely

### Pattern in spawn_image_watcher:

```rust
pub fn spawn_image_watcher(
    _app: Arc<VipsApp>,
    _images: Arc<TokioRwlock<HashMap<String, Arc/ImageFile>>>,
    _image_list: Arc<TokioRwlock<Vec<Arc/ImageFile>>>),
) -> RecommendedWatcher {
    let mut watcher = notify::recommended_watcher(
        move |_res| {},
    ).expect("Failed to create file watcher");

    // Watch parent directory recursively  
    watcher.watch(&PathBuf::from("./images"), RecursiveMode::Recursive).ok();

    watcher
}
```

### Important Implementation Details:

1. **Clone Arcs before task spawning**: The `_app`, `images`, and `image_list` must be cloned in the sync callback BEFORE moving into `tokio::task::spawn()` - otherwise you'll have lifetime issues with the async move block trying to capture them.

2. **Handle both success/error cases**: Use a match statement or explicit error handling on scan results, not just direct tuple destructuring.

3. **VipsImage is not Send-safe**: Must load images in blocking context (either `spawn_blocking` or sync callback before async task), never directly from inside the async block.

4. **Atomic state updates**: When updating map and list, do it atomically to avoid stale data between reads/writes.

5. **Filename extraction**: For CREATE/REMOVE events, extract filename from path; for MODIFY events, check if title changed in existing entry.

## File Structure

- `spawn_image_watcher`: Creates the watcher with proper event handling
- `scan_images`: Initial synchronous scan (used before tokio runtime starts)
- No polling loop needed - notify handles all file changes automatically
