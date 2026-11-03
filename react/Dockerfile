# React 前端镜像:构建期 Vite 构建,运行期 nginx 反代 /api、/i 到
# API_TARGET(默认 http://localhost:8080;连线上 API 时设
# -e API_TARGET=https://yyuqmndqpnjr.sealosbja.site)。前端同源调用,
# 无需 CORS 配置。
FROM node:20-bookworm-slim AS build
WORKDIR /app
COPY package.json package-lock.json* ./
RUN npm ci --no-audit --no-fund
COPY . .
# Vditor 运行时资源(prebuild 会拷贝 public/vditor)+ 构建
ARG VITE_SUGGEST_LANG=""
ENV VITE_SUGGEST_LANG=$VITE_SUGGEST_LANG
RUN npm run build

FROM nginx:1.27-alpine
COPY --from=build /app/dist /usr/share/nginx/html
# 模板在容器启动时经 envsubst 生成 default.conf(${API_TARGET})
COPY nginx.default.conf.template /etc/nginx/templates/default.conf.template
ENV API_TARGET=http://localhost:8080
EXPOSE 80
