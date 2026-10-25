const EXTENDED_UNC_PREFIX = "\\\\?\\UNC\\";
const EXTENDED_PATH_PREFIX = "\\\\?\\";

export function displayPath(path: string) {
  if (path.slice(0, EXTENDED_UNC_PREFIX.length).toLowerCase() === EXTENDED_UNC_PREFIX.toLowerCase()) {
    return `\\\\${path.slice(EXTENDED_UNC_PREFIX.length)}`;
  }
  return path.startsWith(EXTENDED_PATH_PREFIX) ? path.slice(EXTENDED_PATH_PREFIX.length) : path;
}
