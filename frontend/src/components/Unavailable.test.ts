import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import Unavailable from './Unavailable.vue'

describe('Unavailable', () => {
  it('says which page is missing and which operation the hub lacks', () => {
    const wrapper = mount(Unavailable, { props: { what: 'Factory', op: 'factory.task.list' } })

    expect(wrapper.attributes('role')).toBe('status')
    expect(wrapper.text()).toContain('Factory not available')
    expect(wrapper.text()).toContain('factory.task.list')
  })
})
