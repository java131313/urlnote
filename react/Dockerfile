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
# 对象存储文件库(/oss 页)的构建期凭据,与 Rust 前端的 OSS_* 编译期常量同机制;
# 不设也能构建,/oss 页只显示“未配置”。注意:它们会被编译进公开的前端包。
ARG VITE_OSS_ENDPOINT=""
ARG VITE_OSS_AK=""
ARG VITE_OSS_SK=""
ARG VITE_OSS_BUCKET=""
ARG VITE_OSS_REGION=""
ENV VITE_OSS_ENDPOINT=$VITE_OSS_ENDPOINT \
    VITE_OSS_AK=$VITE_OSS_AK \
    VITE_OSS_SK=$VITE_OSS_SK \
    VITE_OSS_BUCKET=$VITE_OSS_BUCKET \
    VITE_OSS_REGION=$VITE_OSS_REGION
RUN npm run build

FROM nginx:1.27-alpine
COPY --from=build /app/dist /usr/share/nginx/html
# 模板在容器启动时经 envsubst 生成 default.conf(${API_TARGET})
COPY nginx.default.conf.template /etc/nginx/templates/default.conf.template
ENV API_TARGET=https://yyuqmndqpnjr.sealosbja.site
EXPOSE 80
