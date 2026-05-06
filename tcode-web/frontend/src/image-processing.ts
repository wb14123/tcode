const MAX_INPUT_SIZE = 10 * 1024 * 1024; // 10 MB — reject above this
const MAX_OUTPUT_SIZE = 2 * 1024 * 1024; // 2 MB — compress target
const MAX_DIMENSION = 2000;
const QUALITY_STEPS = [85, 80, 70, 60, 50, 40];
const MAX_ATTEMPTS = 12; // up to 2 full quality passes at reducing dimensions

/**
 * Encode `bitmap` to a JPEG `Blob` at the given dimensions and quality.
 * Fills the canvas with white first so transparent pixels become white
 * instead of black (JPEG has no alpha channel).
 */
async function encodeJpeg(
  bitmap: ImageBitmap,
  width: number,
  height: number,
  quality: number,
): Promise<Blob> {
  const canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;

  const ctx = canvas.getContext('2d', { premultiplyAlpha: 'none' }) as CanvasRenderingContext2D | null;
  if (!ctx) {
    throw new Error('Failed to create canvas context for image processing.');
  }

  // Fill white background so transparent pixels don't become black.
  ctx.fillStyle = '#fff';
  ctx.fillRect(0, 0, width, height);
  ctx.drawImage(bitmap, 0, 0, width, height);

  return new Promise<Blob>((resolve, reject) => {
    canvas.toBlob(
      (b) => {
        if (b) {
          resolve(b);
        } else {
          reject(new Error('Canvas toBlob returned null'));
        }
      },
      'image/jpeg',
      quality / 100,
    );
  });
}

/**
 * Process an image file client-side before upload:
 *   - Reject files > 10 MB
 *   - Convert non-JPEG formats (HEIC, PNG, WebP, etc.) to JPEG
 *   - Resize images exceeding 2000 px (longest edge) down to 2000 px
 *   - JPEG-encode at quality 85, iteratively reducing quality and
 *     dimensions until the output fits under 2 MB
 *   - GIF files are returned as-is to preserve animation
 *   - Small JPEG files already within limits are returned as-is
 */
export async function processImageFile(file: File): Promise<File> {
  if (file.size > MAX_INPUT_SIZE) {
    throw new Error(
      `Image "${file.name}" exceeds the 10 MB size limit (${(file.size / 1024 / 1024).toFixed(1)} MB).`,
    );
  }

  const isGif =
    file.type === 'image/gif' || file.name.toLowerCase().endsWith('.gif');

  // Load the image into a bitmap for size inspection.
  let bitmap: ImageBitmap;
  try {
    bitmap = await createImageBitmap(file, {
      premultiplyAlpha: 'none',
    });
  } catch {
    throw new Error(
      `Unable to decode image "${file.name}". The format may not be supported by your browser.${isGif ? '' : ' Try converting to JPEG or PNG first.'}`,
    );
  }

  const { width, height } = bitmap;

  // GIF: preserve as-is (animation frames would be lost on re-encode).
  if (isGif) {
    bitmap.close();
    return file;
  }

  // Determine if re-encoding is needed.
  const isJpeg =
    file.type === 'image/jpeg' ||
    file.type === 'image/jpg' ||
    file.name.toLowerCase().endsWith('.jpg') ||
    file.name.toLowerCase().endsWith('.jpeg');

  const needsResize = width > MAX_DIMENSION || height > MAX_DIMENSION;
  const needsCompression = file.size > MAX_OUTPUT_SIZE;
  const needsConversion = !isJpeg;

  if (!needsConversion && !needsResize && !needsCompression) {
    bitmap.close();
    return file;
  }

  // Target dimensions — clamp to MAX_DIMENSION on longest edge.
  const origWidth = needsResize
    ? Math.round(width * (MAX_DIMENSION / Math.max(width, height)))
    : width;
  const origHeight = needsResize
    ? Math.round(height * (MAX_DIMENSION / Math.max(width, height)))
    : height;

  // Try encoding, cycling quality steps then scaling dimensions down.
  // Each full pass through QUALITY_STEPS at one scale level, then
  // reduce dimensions by 10 % and try again.
  let scale = 1.0;
  for (let attempt = 0; attempt < MAX_ATTEMPTS; attempt++) {
    const qualityIndex = attempt % QUALITY_STEPS.length;
    // Scale down after each full quality pass (skip first pass).
    if (attempt > 0 && qualityIndex === 0) {
      scale *= 0.9;
    }

    const targetWidth = Math.max(1, Math.round(origWidth * scale));
    const targetHeight = Math.max(1, Math.round(origHeight * scale));
    const quality = QUALITY_STEPS[qualityIndex]!;

    let blob: Blob;
    try {
      blob = await encodeJpeg(bitmap, targetWidth, targetHeight, quality);
    } catch (err) {
      // Cross-origin taint produces a SecurityError; give a helpful message.
      if (err instanceof DOMException && err.name === 'SecurityError') {
        bitmap.close();
        throw new Error(
          `Cannot process "${file.name}" due to browser security restrictions. Try saving the image and uploading the file directly.`,
        );
      }
      bitmap.close();
      throw err;
    }

    if (blob.size <= MAX_OUTPUT_SIZE) {
      bitmap.close();
      const baseName = file.name.replace(/\.[^.]+$/, '') || 'image';
      return new File([blob], `${baseName}.jpg`, { type: 'image/jpeg' });
    }
  }

  bitmap.close();
  throw new Error(
    `Unable to compress image "${file.name}" to under 2 MB. Try using a smaller or lower-resolution image.`,
  );
}
