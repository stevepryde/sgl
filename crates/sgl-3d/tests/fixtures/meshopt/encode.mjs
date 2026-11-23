// Writes attribute streams long enough to span several blocks, encoded by
// meshoptimizer's own encoder (npm `meshoptimizer` 1.3.0, MIT) at version
// 0 and 1: `stride4-v{0,1}.bin` (1000 elements of 4 bytes: 4 blocks of 256
// and a short one) and `stride64-v{0,1}.bin` (300 elements of 64 bytes: 3
// blocks of 128 and a short one). Version 1 streams use the encoder's
// highest level (3), which also tries the XOR channel mode. `content/meshopt/tests.rs` authors the
// same elements. Run where `meshoptimizer` 1.3.0 resolves:
// `bun encode.mjs`.
import { MeshoptEncoder } from 'meshoptimizer';
import { writeFileSync } from 'node:fs';

await MeshoptEncoder.ready;

// Element i of 4 bytes: a byte ramp, a slow wave, noise and a constant.
function stride4(count) {
	const out = new Uint8Array(count * 4);
	for (let i = 0; i < count; i++) {
		out.set([i & 0xff, (i >> 2) & 0xff, (i * 151 + 7) % 251, 9], i * 4);
	}
	return out;
}

// Element i of 64 bytes: sixteen 4-byte channels, by channel c % 4: a
// u32 counter, two u16 ramps, an f32 (exact in binary) and four bytes.
function stride64(count) {
	const out = new Uint8Array(count * 64);
	const view = new DataView(out.buffer);
	for (let i = 0; i < count; i++) {
		for (let c = 0; c < 16; c++) {
			const at = i * 64 + c * 4;
			switch (c % 4) {
				case 0:
					view.setUint32(at, i * 1000 + c, true);
					break;
				case 1:
					view.setUint16(at, i * 3 + c, true);
					view.setUint16(at + 2, 60000 - i * 5, true);
					break;
				case 2:
					view.setFloat32(at, i * 0.25 + c, true);
					break;
				default:
					out.set([(i * c) & 0xff, i % 7, 255 - (i & 0xff), c], at);
			}
		}
	}
	return out;
}

for (const [name, data, count, stride] of [
	['stride4', stride4(1000), 1000, 4],
	['stride64', stride64(300), 300, 64],
]) {
	for (const version of [0, 1]) {
		const encoded = version === 0
			? MeshoptEncoder.encodeGltfBuffer(data, count, stride, 'ATTRIBUTES', 0)
			: MeshoptEncoder.encodeVertexBufferLevel(data, count, stride, 3, 1);
		writeFileSync(`${name}-v${version}.bin`, encoded);
	}
}
