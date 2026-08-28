export function ProductPreview() {
  return (
    <section className="preview-v2" aria-labelledby="preview-heading">
      <figure className="preview-figure-v2">
        <div className="preview-shot-v2">
          <img
            src="/pix-overview.png"
            alt="Pix overview: a phone or tablet sends text prompts, image attachments, and skills to Pix Host on your Mac or Linux computer, over a direct connection or an encrypted relay."
            width={2364}
            height={1932}
            decoding="async"
            fetchPriority="high"
          />
        </div>
        <figcaption className="preview-caption-v2">
          <span>01 / Overview</span>
          <span id="preview-heading">Your Pi coding agent, in your pocket.</span>
        </figcaption>
      </figure>
    </section>
  )
}
