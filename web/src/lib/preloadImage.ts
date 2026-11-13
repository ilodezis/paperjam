/* Pictures a screen is about to show, fetched and decoded before the screen opens: decoding a
   scan's page while its motion is running would hold the motion's first frame back. */

const held = new Map<string, HTMLImageElement>();
const HELD_MAX = 6;

/** Resolves once the picture is in the browser's cache and decoded; also when it cannot be
 *  (the screen then shows its own fallback). The last few are held so they are not dropped
 *  between the preload and the screen. */
export function preloadImage(url: string): Promise<void> {
  const known = held.get(url);
  if (known !== undefined) return known.decode().then(() => undefined, () => undefined);
  const image = new Image();
  image.decoding = "async";
  image.src = url;
  held.set(url, image);
  if (held.size > HELD_MAX) held.delete(held.keys().next().value as string);
  return image.decode().then(() => undefined, () => undefined);
}
