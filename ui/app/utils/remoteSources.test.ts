import { describe, expect, it } from 'vitest'
import { remoteSources } from './remoteSources'

describe('remoteSources', () => {
  it('opens every source once the images are shown', () => {
    expect(remoteSources(true, ['seloger.com'])).toBe(' https: http:')
  })

  it('opens only the approved domains and their subdomains otherwise', () => {
    expect(remoteSources(false, ['salesforce.com'])).toBe(' https://salesforce.com https://*.salesforce.com http://salesforce.com http://*.salesforce.com')
    expect(remoteSources(false, [])).toBe('')
  })

  it('never lets a host write into the policy', () => {
    expect(remoteSources(false, ['evil.example; script-src *', '*', 'ok.example'])).toBe(' https://ok.example https://*.ok.example http://ok.example http://*.ok.example')
  })
})
