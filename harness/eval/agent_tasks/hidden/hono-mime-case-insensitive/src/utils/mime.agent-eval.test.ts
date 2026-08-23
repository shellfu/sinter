import { getMimeType } from './mime'

describe('agent-eval mime case', () => {
  it('matches extensions case-insensitively', () => {
    expect(getMimeType('LOGO.PNG')).toBe('image/png')
    expect(getMimeType('Index.HTML')).toBe('text/html; charset=utf-8')
    expect(getMimeType('a.Json')).toBe('application/json; charset=utf-8')
  })
})
