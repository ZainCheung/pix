import { ArrowRight, Lock, Radio, Server, Smartphone, Wifi } from 'lucide-react'
import type { ReactNode } from 'react'

function FlowNode({ label, detail, icon }: { label: string; detail: string; icon: ReactNode }) {
  return (
    <div className="flow-node">
      <span className="flow-node-icon">{icon}</span>
      <span>
        <strong>{label}</strong>
        <small>{detail}</small>
      </span>
    </div>
  )
}

export function HowItWorks() {
  return (
    <section className="section-shell how-section" id="how-it-works" aria-labelledby="how-heading">
      <div className="section-intro">
        <div className="section-kicker">03 / Connection paths</div>
        <h2 id="how-heading">Direct when possible. Encrypted when remote. Local always.</h2>
        <p>
          Pix uses the shortest trustworthy path between a paired device and
          your host. The relay is a rendezvous point, not a place where your
          session content lives.
        </p>
      </div>

      <div className="flow-diagram">
        <div className="flow-row flow-row-direct">
          <div className="flow-row-label">
            <Wifi size={16} />
            <span>same network</span>
          </div>
          <FlowNode label="Pix client" detail="phone or Mac" icon={<Smartphone size={17} />} />
          <div className="flow-connector flow-connector-direct">
            <span>Bonjour / TCP</span>
            <ArrowRight size={17} />
          </div>
          <FlowNode label="Pix host" detail="your machine" icon={<Server size={17} />} />
          <div className="flow-pi-node"><span>Pi</span><small>session</small></div>
        </div>

        <div className="flow-row flow-row-remote">
          <div className="flow-row-label">
            <Radio size={16} />
            <span>away from home</span>
          </div>
          <FlowNode label="Pix client" detail="paired device" icon={<Smartphone size={17} />} />
          <div className="flow-connector flow-connector-remote">
            <span>encrypted frames</span>
            <ArrowRight size={17} />
          </div>
          <div className="relay-node">
            <span className="relay-node-icon"><Lock size={16} /></span>
            <span><strong>Pix relay</strong><small>opaque forwarding only</small></span>
          </div>
          <div className="flow-connector flow-connector-remote flow-connector-last">
            <span>encrypted frames</span>
            <ArrowRight size={17} />
          </div>
          <FlowNode label="Pix host" detail="your machine" icon={<Server size={17} />} />
        </div>
      </div>

      <div className="flow-notes">
        <span><i className="note-marker note-marker-accent" />Direct LAN is preferred.</span>
        <span><i className="note-marker" />Relay cannot read application payloads.</span>
        <span><i className="note-marker" />Pi keeps running if reachability changes.</span>
      </div>
    </section>
  )
}
