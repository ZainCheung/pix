export function ProductPreview() {
  return (
    <section className="preview-v2" aria-labelledby="preview-heading">
      <figure className="preview-figure-v2">
        <div className="preview-shot-v2">
          <iframe
            className="preview-video-v2"
            width="560"
            height="315"
            src="https://www.youtube.com/embed/OLZ0yUpsOD0?si=i3UOg0TbmbI3Q2XX"
            title="YouTube video player"
            frameBorder="0"
            allow="accelerometer; autoplay; clipboard-write; encrypted-media; gyroscope; picture-in-picture; web-share"
            referrerPolicy="strict-origin-when-cross-origin"
            allowFullScreen
            loading="lazy"
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
