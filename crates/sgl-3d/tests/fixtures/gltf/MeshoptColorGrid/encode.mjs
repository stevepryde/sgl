// Writes MeshoptColorGrid.glb: a 3x3 grid of vertices whose positions,
// normals, colours and triangle-list indices are authored here and in
// `content/gltf/meshopt_tests.rs`, compressed with meshoptimizer's own
// encoder (npm `meshoptimizer` 1.3.0, MIT) as KHR_meshopt_compression:
// positions and normals in the version 1 attribute stream, colours through
// the COLOR filter (8 bits), indices in the INDICES mode. Run it where
// `meshoptimizer` 1.3.0 resolves: `bun encode.mjs MeshoptColorGrid.glb`.
import { MeshoptEncoder } from 'meshoptimizer';
import { writeFileSync } from 'node:fs';

await MeshoptEncoder.ready;

const positions = [];
const normals = [];
const colours = [];
for (let y = 0; y < 3; y++) {
	for (let x = 0; x < 3; x++) {
		positions.push(x, y, 0);
		normals.push(0, 0, 1);
		colours.push(x / 2, y / 2, (x + y) / 4, 1 - x / 4);
	}
}
const indices = [];
for (let y = 0; y < 2; y++) {
	for (let x = 0; x < 2; x++) {
		const a = y * 3 + x;
		indices.push(a, a + 1, a + 4, a, a + 4, a + 3);
	}
}

const bytes = (typed) => new Uint8Array(typed.buffer, typed.byteOffset, typed.byteLength);
const views = [
	{ data: bytes(new Float32Array(positions)), count: 9, stride: 12, mode: 'ATTRIBUTES' },
	{ data: bytes(new Float32Array(normals)), count: 9, stride: 12, mode: 'ATTRIBUTES' },
	{
		data: MeshoptEncoder.encodeFilterColor(new Float32Array(colours), 9, 4, 8),
		count: 9,
		stride: 4,
		mode: 'ATTRIBUTES',
		filter: 'COLOR',
	},
	{ data: bytes(new Uint16Array(indices)), count: 24, stride: 2, mode: 'INDICES' },
];

const align = (n) => (n + 3) & ~3;
let bin = new Uint8Array(0);
let fallback = 0;
const bufferViews = views.map((view) => {
	const encoded = MeshoptEncoder.encodeGltfBuffer(view.data, view.count, view.stride, view.mode, 1);
	const offset = bin.length;
	const next = new Uint8Array(align(offset + encoded.length));
	next.set(bin);
	next.set(encoded, offset);
	bin = next;
	const json = {
		buffer: 1,
		byteOffset: fallback,
		byteLength: view.count * view.stride,
		extensions: {
			KHR_meshopt_compression: {
				buffer: 0,
				byteOffset: offset,
				byteLength: encoded.length,
				byteStride: view.stride,
				count: view.count,
				mode: view.mode,
				...(view.filter ? { filter: view.filter } : {}),
			},
		},
	};
	if (view.mode === 'ATTRIBUTES') json.byteStride = view.stride;
	fallback = align(fallback + view.count * view.stride);
	return json;
});

const gltf = {
	asset: { version: '2.0', generator: 'SGL meshopt fixture (encode.mjs)' },
	extensionsUsed: ['KHR_meshopt_compression'],
	extensionsRequired: ['KHR_meshopt_compression'],
	scene: 0,
	scenes: [{ nodes: [0] }],
	nodes: [{ mesh: 0 }],
	meshes: [{ primitives: [{ attributes: { POSITION: 0, NORMAL: 1, COLOR_0: 2 }, indices: 3 }] }],
	accessors: [
		{ bufferView: 0, componentType: 5126, count: 9, type: 'VEC3', min: [0, 0, 0], max: [2, 2, 0] },
		{ bufferView: 1, componentType: 5126, count: 9, type: 'VEC3' },
		{ bufferView: 2, componentType: 5121, normalized: true, count: 9, type: 'VEC4' },
		{ bufferView: 3, componentType: 5123, count: 24, type: 'SCALAR' },
	],
	bufferViews,
	buffers: [
		{ byteLength: bin.length },
		{ byteLength: fallback, extensions: { KHR_meshopt_compression: { fallback: true } } },
	],
};

let json = new TextEncoder().encode(JSON.stringify(gltf));
const padded = new Uint8Array(align(json.length)).fill(0x20);
padded.set(json);
json = padded;
const glb = new Uint8Array(12 + 8 + json.length + 8 + bin.length);
const header = new DataView(glb.buffer);
header.setUint32(0, 0x46546c67, true);
header.setUint32(4, 2, true);
header.setUint32(8, glb.length, true);
header.setUint32(12, json.length, true);
header.setUint32(16, 0x4e4f534a, true);
glb.set(json, 20);
header.setUint32(20 + json.length, bin.length, true);
header.setUint32(24 + json.length, 0x004e4942, true);
glb.set(bin, 28 + json.length);
writeFileSync(process.argv[2], glb);
