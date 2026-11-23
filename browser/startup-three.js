// three.js r187's side of the startup comparison
// (examples/sgl-3d/BROWSER_BASELINE.md, "Startup against three.js"), bundled
// and minified with this module by the runner. It uses each feature as
// three.js documents it, matched to SGL's settings for the same profile.
import * as THREE from "three/webgpu";
import { builtinAOContext, mrt, normalView, output, pass, renderOutput, screenUV, velocity } from "three/tsl";
import { bloom } from "three/addons/tsl/display/BloomNode.js";
import { ao } from "three/addons/tsl/display/GTAONode.js";
import { sharpen } from "three/addons/tsl/display/SharpenNode.js";
import { traa } from "three/addons/tsl/display/TRAANode.js";
import { CSMShadowNode } from "three/addons/csm/CSMShadowNode.js";
import { GLTFLoader } from "three/addons/loaders/GLTFLoader.js";
import { run } from "./startup-harness.js";

let renderer, gltf, scene, camera, layout, pipeline, scenePass, aoContext, mask;
const moving = [];
const errors = [];
const rotation = new THREE.Matrix4();

run({
  library: "three",
  async init() {},
  async device(canvas) {
    renderer = new THREE.WebGPURenderer({ canvas, antialias: false });
    renderer.setPixelRatio(1);
    renderer.setSize(canvas.width, canvas.height, false);
    await renderer.init();
    renderer.toneMapping = THREE.NeutralToneMapping;
    const device = renderer.backend.isWebGPUBackend === true ? renderer.backend.device : null;
    device?.addEventListener("uncapturederror", (event) => errors.push(event.error.message));
    return {
      backend: device ? "WebGPU" : renderer.backend.isWebGLBackend ? "WebGL2" : null,
      revision: THREE.REVISION,
      adapter: device ? Object.fromEntries(["vendor", "architecture", "device", "description"].map((k) => [k, device.adapterInfo[k]])) : null,
    };
  },
  async decode(glb) {
    gltf = await new GLTFLoader().parseAsync(glb.buffer.slice(glb.byteOffset, glb.byteOffset + glb.byteLength), "");
  },
  async upload(plan) {
    layout = plan;
    const opening = layout.profile === "opening";
    renderer.shadowMap.enabled = opening;
    scene = new THREE.Scene();
    scene.background = new THREE.Color(0);
    const nodes = await Promise.all(Array.from({ length: layout.prototypes + 1 }, (_, i) => gltf.parser.getDependency("node", i)));
    for (const node of nodes) if (opening && node.material.map) node.material.map.anisotropy = 8;
    const place = (prototype, matrix) => {
      const mesh = nodes[prototype].clone();
      mesh.matrixAutoUpdate = false;
      mesh.matrix.fromArray(matrix);
      mesh.castShadow = mesh.receiveShadow = opening;
      return mesh;
    };
    for (const instance of layout.instances) {
      const mesh = place(instance.prototype, instance.matrix);
      scene.add(mesh);
      if (instance.moving) moving.push({ mesh, pose: new THREE.Matrix4().fromArray(instance.matrix) });
    }
    mask = place(layout.mask_prototype, layout.mask_matrix);
    for (const light of layout.lights) {
      const point = new THREE.PointLight(new THREE.Color(...light.color), light.intensity, light.range, 2);
      point.position.fromArray(light.position);
      scene.add(point);
    }
    const sun = new THREE.DirectionalLight(new THREE.Color(...layout.sun.color), layout.sun.illuminance);
    sun.position.fromArray(layout.sun.direction).multiplyScalar(-50);
    scene.add(sun, sun.target);
    if (layout.sun.shadow) {
      sun.castShadow = true;
      sun.shadow.mapSize.set(2048, 2048);
      sun.shadow.shadowNode = new CSMShadowNode(sun, { cascades: layout.sun.shadow.cascades, maxFar: layout.sun.shadow.distance });
    }
    const h = layout.hemisphere;
    scene.add(new THREE.HemisphereLight(new THREE.Color(...h.sky), new THREE.Color(...h.ground), h.intensity));
    const c = layout.camera;
    camera = new THREE.PerspectiveCamera(c.fov_y_degrees, layout.size[0] / layout.size[1], c.near, c.far);
  },
  async view() {
    if (layout.profile !== "opening") return;
    // TAA needs velocity; bloom before it, RCAS sharpening after tone mapping, as SGL orders them.
    scenePass = pass(scene, camera);
    scenePass.setMRT(mrt({ output, velocity }));
    const color = scenePass.getTextureNode("output");
    const antialiased = traa(color.add(bloom(color)), scenePass.getTextureNode("depth"), scenePass.getTextureNode("velocity"), camera);
    pipeline = new THREE.RenderPipeline(renderer);
    pipeline.outputColorTransform = false;
    pipeline.outputNode = sharpen(renderOutput(antialiased));
  },
  async prepare() {
    await renderer.compileAsync(scene, camera);
  },
  frame(seconds, state) {
    for (const { mesh, pose } of moving) mesh.matrix.copy(pose).multiply(rotation.makeRotationY(seconds));
    if (state.mask !== (mask.parent === scene)) state.mask ? scene.add(mask) : scene.remove(mask);
    const c = layout.camera;
    const target = new THREE.Vector3().fromArray(c.target);
    camera.position.fromArray(c.eye).sub(target).applyAxisAngle(new THREE.Vector3(0, 1, 0), THREE.MathUtils.degToRad(state.yaw)).add(target);
    camera.lookAt(target);
    if (pipeline) {
      // Ambient occlusion as GTAONode documents it: a normal and depth prepass feeding the lighting.
      if (state.effect !== (scenePass.contextNode !== null)) {
        if (state.effect && !aoContext) {
          const prePass = pass(scene, camera);
          prePass.setMRT(mrt({ output: normalView }));
          const occlusion = ao(prePass.getTextureNode("depth"), prePass.getTextureNode(), camera);
          aoContext = builtinAOContext(occlusion.getTextureNode().sample(screenUV).r);
        }
        scenePass.contextNode = state.effect ? aoContext : null;
        pipeline.needsUpdate = true;
      }
      pipeline.render();
    } else {
      renderer.render(scene, camera);
    }
  },
  completion() {
    return renderer.backend.device.queue.onSubmittedWorkDone();
  },
  details() {
    return { revision: THREE.REVISION, errors, renderer_info: { render_calls: renderer.info.render.calls, programs: renderer.info.programs?.length ?? null } };
  },
});
