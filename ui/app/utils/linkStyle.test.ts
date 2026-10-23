import { describe, expect, it } from 'vitest'
import { generatedText, invisibleColor } from './linkStyle'

describe('invisibleColor', () => {
  it('finds text nobody can read', () => {
    expect(invisibleColor('transparent')).toBe(true)
    expect(invisibleColor('rgba(0, 0, 0, 0)')).toBe(true)
    expect(invisibleColor('rgb(0 0 0 / 0)')).toBe(true)
  })
  it('leaves readable colours', () => {
    expect(invisibleColor('rgb(20, 20, 20)')).toBe(false)
    expect(invisibleColor('rgba(0, 0, 0, 0.6)')).toBe(false)
    expect(invisibleColor('rgb(0 0 0 / 0.5)')).toBe(false)
  })
})

describe('generatedText', () => {
  it('sees text written by a style', () => {
    expect(generatedText('"paypal.com"')).toBe(true)
    expect(generatedText('attr(data-x)')).toBe(true)
  })
  it('ignores pseudo-elements without text', () => {
    for (const none of ['none', 'normal', '""', '']) expect(generatedText(none)).toBe(false)
  })
})
