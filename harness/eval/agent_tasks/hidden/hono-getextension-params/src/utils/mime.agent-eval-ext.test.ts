import { getExtension } from './mime'

describe('agent-eval getExtension params', () => {
  it('ignores parameters', () => {
    expect(getExtension('text/html; charset=utf-8')).toBe('html')
    expect(getExtension('application/json;charset=utf-8')).toBe('json')
    expect(getExtension('image/png')).toBe('png')
  })
})
