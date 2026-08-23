import { defaultMatch, parseAccept } from './accepts'

describe('agent-eval accepts wildcard', () => {
  const config = { header: 'Accept' as const, supports: ['application/json', 'text/html'], default: 'text/plain' }
  it('*/* matches first supported', () => {
    expect(defaultMatch(parseAccept('*/*'), config)).toBe('application/json')
    expect(defaultMatch(parseAccept('*'), config)).toBe('application/json')
  })
  it('explicit match still wins', () => {
    expect(defaultMatch(parseAccept('text/html, */*;q=0.1'), config)).toBe('text/html')
  })
  it('no match falls to default', () => {
    expect(defaultMatch(parseAccept('image/png'), config)).toBe('text/plain')
  })
})
