<template>
  <div ref="host" class="viewer3d" role="img" :aria-label="`3D model ${title}`">
    <p v-if="error" class="error-box small">{{ error }}</p>
    <p v-else-if="!ready" class="muted small pad">Loading the 3D model...</p>
    <p v-if="ready && !error" class="hint muted small">Drag to turn, scroll to zoom, right-drag to move</p>
  </div>
</template>

<script setup lang="ts">
// A 3D model on a WebGL canvas: GLB, glTF, OBJ and STL, with orbit controls. three.js is loaded only when a model is shown.
import { onBeforeUnmount, onMounted, ref, watch } from 'vue'

const props = defineProps<{ url: string; format: 'glb' | 'gltf' | 'obj' | 'stl'; title: string }>()
const host = ref<HTMLElement | null>(null)
const ready = ref(false)
const error = ref('')
let dispose: (() => void) | null = null

async function show() {
  dispose?.()
  dispose = null
  ready.value = false
  error.value = ''
  const el = host.value
  if (!el) return
  try {
    const THREE = await import('three')
    const { OrbitControls } = await import('three/examples/jsm/controls/OrbitControls.js')
    const scene = new THREE.Scene()
    const camera = new THREE.PerspectiveCamera(45, 1, 0.01, 1000)
    const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true })
    renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2))
    el.appendChild(renderer.domElement)
    scene.add(new THREE.HemisphereLight(0xffffff, 0x445566, 1.6))
    const sun = new THREE.DirectionalLight(0xffffff, 2.2)
    sun.position.set(3, 5, 4)
    scene.add(sun)
    const controls = new OrbitControls(camera, renderer.domElement)
    controls.enableDamping = true

    let object: any
    if (props.format === 'glb' || props.format === 'gltf') {
      const { GLTFLoader } = await import('three/examples/jsm/loaders/GLTFLoader.js')
      object = (await new GLTFLoader().loadAsync(props.url)).scene
    } else if (props.format === 'obj') {
      const { OBJLoader } = await import('three/examples/jsm/loaders/OBJLoader.js')
      object = await new OBJLoader().loadAsync(props.url)
    } else {
      const { STLLoader } = await import('three/examples/jsm/loaders/STLLoader.js')
      const geometry = await new STLLoader().loadAsync(props.url)
      geometry.computeVertexNormals()
      object = new THREE.Mesh(geometry, new THREE.MeshStandardMaterial({ color: 0xb8c4d6, roughness: 0.55, metalness: 0.1 }))
    }
    scene.add(object)
    // frame it: centre at the origin, camera back far enough to see all of it
    const box = new THREE.Box3().setFromObject(object)
    const size = box.getSize(new THREE.Vector3())
    const centre = box.getCenter(new THREE.Vector3())
    object.position.sub(centre)
    const radius = Math.max(size.x, size.y, size.z, 1e-6)
    camera.position.set(radius * 1.2, radius * 0.8, radius * 1.6)
    camera.near = radius / 100
    camera.far = radius * 100
    camera.updateProjectionMatrix()
    controls.update()

    const resize = () => {
      const w = el.clientWidth || 300
      const h = el.clientHeight || 300
      renderer.setSize(w, h, false)
      renderer.domElement.style.width = '100%'
      renderer.domElement.style.height = '100%'
      camera.aspect = w / h
      camera.updateProjectionMatrix()
    }
    resize()
    const ro = new ResizeObserver(resize)
    ro.observe(el)
    let frame = 0
    const loop = () => {
      frame = requestAnimationFrame(loop)
      controls.update()
      renderer.render(scene, camera)
    }
    loop()
    ready.value = true
    dispose = () => {
      cancelAnimationFrame(frame)
      ro.disconnect()
      controls.dispose()
      renderer.dispose()
      renderer.domElement.remove()
    }
  } catch (e: any) {
    error.value = `This model could not be shown: ${e?.message || e}`
  }
}

onMounted(show)
watch(() => [props.url, props.format], show)
onBeforeUnmount(() => dispose?.())
</script>

<style scoped>
.viewer3d { position: relative; width: 100%; height: 100%; min-height: 280px; background: radial-gradient(circle at 50% 40%, var(--panel-2), var(--bg)); border-radius: 8px; overflow: hidden; }
.viewer3d :deep(canvas) { display: block; }
.hint { position: absolute; left: 10px; bottom: 6px; pointer-events: none; }
.pad { padding: 12px; }
</style>
