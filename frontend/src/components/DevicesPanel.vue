<template>
  <section class="card" aria-label="Phones and other devices">
    <h3>Phones and other devices</h3>
    <p class="muted small">
      Another ModelMistress, such as the one on a phone, can receive a model from this hub. Enter its address and the key
      it shows under Connect software, then save and check it. A model is sent from the Installed tab with Send to phone.
    </p>
    <p v-if="error" class="error-box" role="alert">{{ error }}</p>
    <p v-if="message" class="muted small" role="status">{{ message }}</p>

    <p v-if="!devices.length" class="muted small">No devices yet.</p>
    <div v-for="(d, i) in devices" :key="i" class="connect-tool">
      <div class="row">
        <label class="grow">
          Name
          <input v-model="d.name" type="text" maxlength="64" />
        </label>
        <label class="grow">
          Address
          <input v-model="d.url" type="text" placeholder="http://192.0.2.10:35329" />
        </label>
      </div>
      <div class="row">
        <label class="grow">
          Key
          <input v-model="d.key" type="password" autocomplete="off" :placeholder="d.key === '(set)' ? 'Saved. Type a new key to replace it' : 'From the other device'" />
        </label>
        <button
          class="btn"
          type="button"
          :disabled="busy || !isSaved(d.name)"
          :title="isSaved(d.name) ? 'Ask the device whether it answers' : 'Save first, then check'"
          @click="check(d)"
        >Check</button>
      </div>
      <div class="row">
        <span class="grow muted small" role="status">{{ checkText(d.name) }}</span>
        <button class="btn" type="button" :disabled="busy" @click="remove(i)">Remove</button>
      </div>
    </div>

    <div class="row">
      <button class="btn" type="button" :disabled="busy" @click="add">Add a device</button>
      <button class="btn primary" type="button" :disabled="busy" @click="save">Save devices</button>
    </div>
  </section>
</template>

<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { newDevice, type DeviceCheck, type DeviceSetting } from '@/devices'
import { call } from '@/transport'

const devices = ref<DeviceSetting[]>([])
/** The names saved in the settings. A device is checked only once it is saved, since the hub keeps its key. */
const saved = ref<string[]>([])
const checks = ref<Record<string, DeviceCheck>>({})
const busy = ref(false)
const error = ref('')
const message = ref('')

function isSaved(name: string): boolean {
  return saved.value.includes(name)
}

function checkText(name: string): string {
  const c = checks.value[name]
  if (!c) return isSaved(name) ? 'Not checked yet.' : 'Save to check it.'
  return c.reachable ? `Reachable. ${c.answer}` : `Not reachable. ${c.answer}`
}

async function load() {
  try {
    const cfg = await call('config.get', {})
    devices.value = ((cfg.devices || []) as DeviceSetting[]).map((d) => ({ ...d }))
    saved.value = devices.value.map((d) => d.name)
  } catch (e: any) {
    error.value = `The devices did not load: ${e?.message || e}`
  }
}

function add() {
  devices.value.push(newDevice(devices.value))
}

function remove(i: number) {
  devices.value.splice(i, 1)
}

async function save() {
  error.value = ''
  message.value = ''
  busy.value = true
  try {
    await call('config.set', { settings: { devices: devices.value.map((d) => ({ ...d })) } })
    message.value = 'Saved. Check each device to see that it answers.'
    checks.value = {}
    await load()
  } catch (e: any) {
    error.value = `Not saved: ${e?.message || e}`
  } finally {
    busy.value = false
  }
}

async function check(d: DeviceSetting) {
  error.value = ''
  message.value = ''
  busy.value = true
  try {
    checks.value = { ...checks.value, [d.name]: (await call('device.check', { device: d.name })) as DeviceCheck }
  } catch (e: any) {
    error.value = `The check did not run: ${e?.message || e}`
  } finally {
    busy.value = false
  }
}

onMounted(load)
</script>
