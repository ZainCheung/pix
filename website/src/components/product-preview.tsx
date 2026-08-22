import { CircleDot, Folder, Lock, Wifi } from 'lucide-react'

export function ProductPreview() {
  return (
    <section className="preview-v2" aria-labelledby="preview-heading">
      <div className="preview-shot-v2">
        <div className="preview-shot-head-v2">
          <div className="window-dots" aria-hidden="true"><span /><span /><span /></div>
          <span>pix / protocol review</span>
          <span className="preview-online-v2"><CircleDot size={11} /> connected</span>
        </div>
        <div className="preview-shot-body-v2">
          <aside className="preview-sidebar-v2">
            <span className="preview-label-v2">workspace</span>
            <strong><Folder size={14} /> pix</strong>
            <small>~/Projects/pix</small>
            <span className="preview-label-v2 preview-label-spaced-v2">session</span>
            <strong className="preview-session-v2"><span /> protocol review</strong>
            <small className="preview-sidebar-foot-v2"><Wifi size={12} /> LAN preferred</small>
          </aside>
          <div className="preview-transcript-v2">
            <div className="preview-transcript-meta-v2"><span>pi / session attached</span><span>12ms · direct</span></div>
            <p><span className="preview-prompt-v2">you</span> Check the relay boundary against the v1 schema.</p>
            <p className="preview-muted-v2">Reading protocol/schema/v1.md</p>
            <p><span className="preview-prompt-v2 preview-prompt-agent-v2">pi</span> Relay forwards opaque frames. It does not parse application payloads.</p>
            <p className="preview-caret-v2"><span className="preview-prompt-v2">you</span><i /></p>
          </div>
        </div>
        <div className="preview-shot-foot-v2">
          <span><Lock size={12} /> encrypted channel</span>
          <span>authorized workspace</span>
          <span>Pi stays local</span>
        </div>
      </div>
      <div className="preview-caption-v2">
        <span>01 / A session, not a cloud workspace</span>
        <span id="preview-heading">Pi runs here. Pix lets you reach it there.</span>
      </div>
    </section>
  )
}
