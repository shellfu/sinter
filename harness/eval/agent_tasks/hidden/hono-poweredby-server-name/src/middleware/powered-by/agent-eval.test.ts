import { Hono } from '../../hono'
import { poweredBy } from '.'

describe('agent-eval poweredBy serverName', () => {
  it('custom and default', async () => {
    const app = new Hono()
    app.use('/custom/*', poweredBy({ serverName: 'Acme' }))
    app.use('/default/*', poweredBy())
    app.get('/custom/a', (c) => c.text('x'))
    app.get('/default/a', (c) => c.text('x'))
    expect((await app.request('/custom/a')).headers.get('X-Powered-By')).toBe('Acme')
    expect((await app.request('/default/a')).headers.get('X-Powered-By')).toBe('Hono')
  })
})
