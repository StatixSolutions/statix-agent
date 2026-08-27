FROM busybox:1.37

RUN mkdir -p /www && echo 'ok: success' > /www/index.html

EXPOSE 8080

CMD ["httpd", "-f", "-p", "8080", "-h", "/www"]
