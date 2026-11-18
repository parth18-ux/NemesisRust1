// Start Swagger UI on /docs (kept out of the page so its content security policy allows no inline script).
// validatorUrl null: no badge image is fetched from swagger.io.
window.ui = window.SwaggerUIBundle({
  url: "/openapi.json", dom_id: "#swagger-ui", layout: "BaseLayout", deepLinking: true, showExtensions: true,
  showCommonExtensions: true, validatorUrl: null,
  presets: [window.SwaggerUIBundle.presets.apis, window.SwaggerUIBundle.SwaggerUIStandalonePreset],
});
