# urlnote frontend — runtime image only.
#
# The WASM frontend is built by CI (.github/workflows/docker.yml) with Trunk
# before this image is built; nginx just serves the static output in
# rust/frontend/dist. The API origin (API_BASE) is compiled into the WASM and
# points at the Cloudflare worker, so this container serves no /api routes
# and needs no backend alongside.

FROM nginx:1.27-alpine

COPY docker/nginx.frontend.conf /etc/nginx/conf.d/default.conf
COPY rust/frontend/dist /usr/share/nginx/html

EXPOSE 80
