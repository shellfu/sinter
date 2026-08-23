import { parseAccept } from './accepts'

describe('agent-eval parseAccept bare param', () => {
  it('does not throw', () => {
    const r = parseAccept('text/html;level, application/json;q=0.5')
    expect(r[0].type).toBe('text/html')
    expect(r[0].params.level).toBe('')
    expect(r[0].q).toBe(1)
    expect(r[1].q).toBe(0.5)
  })
})
