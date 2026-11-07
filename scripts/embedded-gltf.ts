// Small offline core glTF -> GLB helper for prepared PNG fixtures. No scene registry.
import { mkdir, open, realpath, rename, stat } from "node:fs/promises";
import { createHash } from "node:crypto";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { assertNoInputFileAliases } from "./bistro-export-files";

async function localFile(directory: string, uri: string): Promise<string> {
  const decoded = decodeURIComponent(uri);
  const path = resolve(directory, decoded), local = relative(directory, path);
  if (!decoded || decoded.includes(":") || decoded.includes("\\") || isAbsolute(decoded) || local === ".." || local.startsWith(`..${sep}`)) throw new Error(`URI escapes prepared directory: ${uri}`);
  const actual = await realpath(path), root = await realpath(directory), confined = relative(root, actual);
  if (confined === ".." || confined.startsWith(`..${sep}`) || isAbsolute(confined)) throw new Error(`URI escapes prepared directory through a symlink: ${uri}`);
  return actual;
}

export async function embedPreparedGltf(input: string, destination: string, options: { deduplicateImages?: boolean; suppliedImages?: ReadonlyMap<number, string> } = {}) {
  const prepared = dirname(input);
  const document = await Bun.file(input).json();
  document.bufferViews ??= [];
  document.images ??= [];
  if (options.deduplicateImages) {
    // Unknown extensions can hold image indices we cannot safely rewrite.
    // Image metadata is not thrown away merely because PNG bytes match.
    if (Object.keys(document.extensions ?? {}).length || (document.textures ?? []).some((texture: any) => Object.keys(texture.extensions ?? {}).length)) throw new Error("image deduplication requires core image references without root/texture extensions");
    if (document.images.some((image: any) => Object.keys(image).some(key => !["uri", "mimeType"].includes(key)))) throw new Error("image deduplication requires images without names, extras, extensions or custom metadata");
  }
  // This package has one external geometry buffer. Images are appended to that
  // buffer as standard GLB buffer views; no runtime URI resolver is needed.
  if (document.buffers.length !== 1 || !document.buffers[0].uri) throw new Error("expected one external geometry buffer");
  const geometryPath = await localFile(prepared, document.buffers[0].uri);
  const inputFiles = [input, geometryPath];
  const geometryBytes = (await stat(geometryPath)).size;
  if (geometryBytes !== document.buffers[0].byteLength) throw new Error("geometry byteLength does not match its file");
  const align = (bytes: number) => Math.ceil(bytes / 4) * 4;
  const files = [{ path: geometryPath, bytes: geometryBytes }];
  let binBytes = align(geometryBytes), encodedImageBytes = 0, decodedImageBytes = 0;
  let originalEncodedImageBytes = 0, originalDecodedImageBytes = 0;
  const images: { index: number; name: string; width: number; height: number; encodedBytes: number; decodedBytes: number }[] = [];
  const retainedImages = [], originalToRetainedImages: number[] = [];
  const originalImageUris: string[] = document.images.map((image: any) => image.uri);
  const candidates = new Map<string, { index: number; path: string }[]>();
  for (const image of document.images) {
    const path = await localFile(prepared, image.uri), bytes = (await stat(path)).size;
    inputFiles.push(path);
    const header = new DataView(await Bun.file(path).slice(0, 24).arrayBuffer());
    if (header.getUint32(0) !== 0x89504e47 || header.getUint32(12) !== 0x49484452) throw new Error(`expected prepared PNG: ${image.uri}`);
    const width = header.getUint32(16), height = header.getUint32(20), decodedBytes = width * height * 4;
    originalEncodedImageBytes += bytes;
    originalDecodedImageBytes += decodedBytes;
    const index = images.length;
    if (options.deduplicateImages) {
      const contents = Buffer.from(await Bun.file(path).arrayBuffer());
      const hash = createHash("sha256").update(contents).digest("hex");
      const group = candidates.get(hash) ?? [];
      let duplicate: number | undefined;
      for (const candidate of group) {
        // SHA-256 groups candidates; actual byte equality decides sharing.
        if (contents.equals(Buffer.from(await Bun.file(candidate.path).arrayBuffer()))) {
          duplicate = candidate.index;
          break;
        }
      }
      if (duplicate !== undefined) {
        originalToRetainedImages.push(duplicate);
        continue;
      }
      group.push({ index, path });
      candidates.set(hash, group);
    }
    originalToRetainedImages.push(index);
    retainedImages.push(image);
    images.push({ index, name: image.uri, width, height, encodedBytes: bytes, decodedBytes });
    encodedImageBytes += bytes;
    decodedImageBytes += decodedBytes;
    const supplied = options.suppliedImages?.get(index);
    if (supplied !== undefined) {
      // A game supplies these images through LoadOptions::images, not a glTF
      // extension or runtime transcode. Unsupplied PNGs remain embedded.
      image.uri = supplied;
      delete image.mimeType;
      continue;
    }
    image.bufferView = document.bufferViews.length;
    document.bufferViews.push({ buffer: 0, byteOffset: binBytes, byteLength: bytes });
    image.mimeType = "image/png";
    delete image.uri;
    files.push({ path, bytes });
    binBytes += align(bytes);
  }
  document.images = retainedImages;
  if (options.deduplicateImages) {
    for (const texture of document.textures ?? []) {
      if (texture.source === undefined) continue;
      if (!Number.isInteger(texture.source) || originalToRetainedImages[texture.source] === undefined) throw new Error("texture source is outside the prepared image array");
      // Texture indices, sampler selection and every material textureInfo
      // (including transform and colour/data role) remain exactly as authored.
      texture.source = originalToRetainedImages[texture.source];
    }
  }
  document.buffers[0].byteLength = binBytes;
  delete document.buffers[0].uri;
  const json = Buffer.from(JSON.stringify(document)), jsonBytes = align(json.length), totalBytes = 12 + 8 + jsonBytes + 8 + binBytes;
  if (totalBytes > 0xffff_ffff) throw new Error("GLB exceeds its 32-bit container limit");
  await assertNoInputFileAliases([destination, `${destination}.part`], inputFiles);
  await mkdir(dirname(destination), { recursive: true });
  const file = await open(`${destination}.part`, "w");
  async function append(bytes: Uint8Array): Promise<void> {
    let offset = 0;
    while (offset < bytes.length) {
      const { bytesWritten } = await file.write(bytes, offset, bytes.length - offset);
      if (bytesWritten === 0) throw new Error("GLB write made no progress");
      offset += bytesWritten;
    }
  }
  try {
    const header = Buffer.alloc(20);
    header.write("glTF"); header.writeUInt32LE(2, 4); header.writeUInt32LE(totalBytes, 8);
    header.writeUInt32LE(jsonBytes, 12); header.writeUInt32LE(0x4e4f534a, 16);
    await append(header);
    await append(json);
    await append(Buffer.alloc(jsonBytes - json.length, 0x20));
    const binHeader = Buffer.alloc(8);
    binHeader.writeUInt32LE(binBytes); binHeader.writeUInt32LE(0x004e4942, 4);
    await append(binHeader);
    for (const source of files) {
      for await (const chunk of Bun.file(source.path).stream()) await append(chunk);
      await append(Buffer.alloc(align(source.bytes) - source.bytes));
    }
  } finally { await file.close(); }
  await rename(`${destination}.part`, destination);
  return {
    geometryBytes, encodedImageBytes, decodedImageBytes, glbBytes: totalBytes, binBytes,
    sourceBudgetBytes: totalBytes + binBytes, imageCount: images.length,
    imageDeduplication: {
      enabled: options.deduplicateImages ?? false,
      originalImageCount: originalToRetainedImages.length,
      retainedImageCount: images.length,
      duplicateImagesRemoved: originalToRetainedImages.length - images.length,
      originalEncodedImageBytes, originalDecodedImageBytes,
      encodedImageBytesSaved: originalEncodedImageBytes - encodedImageBytes,
      decodedImageBytesSaved: originalDecodedImageBytes - decodedImageBytes,
      originalImageUris, originalToRetainedImages,
    },
    materialCount: (document.materials ?? []).length,
    meshCount: (document.meshes ?? []).length, nodeCount: (document.nodes ?? []).length,
    maxTextureSide: Math.max(0, ...images.map(image => Math.max(image.width, image.height))), images,
  };
}

if (import.meta.main) {
  const [input, output, option] = Bun.argv.slice(2);
  if (!input || !output || (option && option !== "--deduplicate-images")) throw new Error("usage: bun scripts/embedded-gltf.ts INPUT.gltf OUTPUT.glb [--deduplicate-images]");
  console.log(JSON.stringify(await embedPreparedGltf(resolve(input), resolve(output), { deduplicateImages: option === "--deduplicate-images" }), null, 2));
}
