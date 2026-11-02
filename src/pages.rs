fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn shell(title: &str, content: &str) -> String {
    shell_with_class(title, "", content)
}

fn shell_with_class(title: &str, body_class: &str, content: &str) -> String {
    let (before_title, remainder) = include_str!("../static/page.html")
        .split_once("{{TITLE}}")
        .expect("page title placeholder");
    let (before_class, remainder) = remainder
        .split_once("{{BODY_CLASS}}")
        .expect("body class placeholder");
    let (before_content, after_content) = remainder
        .split_once("{{CONTENT}}")
        .expect("page content placeholder");
    format!(
        "{before_title}{}{before_class}{body_class}{before_content}{content}{after_content}",
        escape(title)
    )
}

/// Inlines the auth config so the shell can boot without a `/api/auth/config` round trip.
pub fn inject_config(html: &str, cookie_login: bool, debug_bearer: bool) -> String {
    let script = format!(
        "<script>window.__FLASHPASTE_CONFIG__={{\"cookie_login\":{cookie_login},\"debug_bearer\":{debug_bearer}}};</script>"
    );
    match html.find("</head>") {
        Some(index) => format!("{}{}{}", &html[..index], script, &html[index..]),
        None => format!("{script}{html}"),
    }
}

pub fn bootstrap(token: &str) -> String {
    shell(
        "首次初始化",
        &format!(
            r#"<section class="card narrow"><div class="icon" aria-hidden="true">✓</div><p class="eyebrow">仅首次启动显示</p><h1>先保存你的 Super Token</h1><p class="lead">系统已初始化。这是首个全局管理凭证，后续可用它创建普通成员 Token。</p><div class="warning">明文只显示一次，离开此页后无法找回。请保存到安全的密码管理器中，不要公开分享。</div><code id="tokenVal" class="secret">{}</code><div class="actions"><button id="copyToken" type="button">复制 Token</button><button id="enterWorkspace" class="primary" type="button">已保存，前往登录</button></div><p id="copyStatus" class="status" role="status"></p><p class="hint">首次登录可选择记住设备；以后会自动验证会话，不会再次显示初始化凭证。</p></section><script>document.getElementById('copyToken').onclick=async()=>{{try{{await navigator.clipboard.writeText(document.getElementById('tokenVal').textContent);document.getElementById('copyStatus').textContent='已复制，请安全保存。'}}catch{{document.getElementById('copyStatus').textContent='无法自动复制，请手动选中并复制上方凭证。'}}}};document.getElementById('enterWorkspace').onclick=()=>location.replace('/');</script>"#,
            escape(token)
        ),
    )
}

pub fn public_view(title: Option<&str>, content_type: &str, content: &str) -> String {
    let title = title
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("无标题分享");
    if content_type == "html" {
        let csp_header = "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; img-src data: blob:\">";
        let srcdoc = escape(&format!("{csp_header}{content}"));
        let script = r#"<script>(()=>{const b=document.getElementById('toggleFullscreen');if(b){b.onclick=()=>{document.body.classList.toggle('fullscreen-active');if(document.body.classList.contains('fullscreen-active')){b.textContent='退出全屏';try{document.documentElement.requestFullscreen()}catch{}}else{b.textContent='全屏沉浸';try{if(document.fullscreenElement)document.exitFullscreen()}catch{}}};document.addEventListener('fullscreenchange',()=>{if(!document.fullscreenElement){document.body.classList.remove('fullscreen-active');b.textContent='全屏沉浸';}});}})();</script>"#;
        return shell_with_class(
            title,
            "viewer-immersive",
            &format!(
                r#"<div class="compact-bar"><div class="compact-bar-left"><a class="compact-brand" href="/" title="FlashPaste 首页">↗</a><span class="compact-title">{}</span><span class="compact-badge">HTML 静态预览</span></div><div class="compact-bar-right"><button class="compact-btn" id="toggleFullscreen" type="button" title="切换全屏沉浸浏览">全屏沉浸</button><a class="compact-btn quiet" href="/">工作台</a></div></div><div class="top-trigger" aria-hidden="true"></div><div class="viewer-container"><iframe id="publicFrame" title="分享内容" sandbox="" srcdoc="{}"></iframe></div>{script}"#,
                escape(title),
                srcdoc
            ),
        );
    }
    let (label, body) = match content_type {
        "markdown" => ("Markdown 原文", format!("<pre>{}</pre>", escape(content))),
        _ => ("纯文本", format!("<pre>{}</pre>", escape(content))),
    };
    shell(
        title,
        &format!(
            r#"<div class="share-heading"><div><span class="badge">公开分享</span><h1>{}</h1><p class="share-note">{} · 任何持有此链接的人都可查看</p></div><a class="button" href="/">登录我的工作台</a></div><section class="card content-card" aria-label="分享内容">{}</section><p class="hint">此内容为限时分享，到期或被删除后将无法访问。</p>"#,
            escape(title),
            label,
            body
        ),
    )
}

pub fn unavailable() -> String {
    shell(
        "内容不可用",
        r#"<section class="card narrow"><div class="icon" aria-hidden="true">↗</div><p class="eyebrow">404 / CONTENT UNAVAILABLE</p><h1>这份内容暂时无法查看</h1><p class="lead">链接可能已过期、内容已删除或焚毁，也可能需要 Token 授权。请确认链接，或联系分享者。</p><div class="actions"><a class="button primary" href="/">返回 Token 登录</a><button type="button" onclick="location.reload()">重新加载</button></div><p class="hint">私有内容请使用获授权的 Token 登录工作台查看。</p></section>"#,
    )
}

pub fn public_external_view(title: Option<&str>, id: &str) -> String {
    let title = title
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("无标题分享");
    let body_url = serde_json::to_string(&format!(
        "/api/contents/{}/body",
        url::form_urlencoded::byte_serialize(id.as_bytes()).collect::<String>()
    ))
    .unwrap()
    .replace('<', "\\u003c");
    let id_escaped = url::form_urlencoded::byte_serialize(id.as_bytes()).collect::<String>();
    let script = r#"<script>(async()=>{const status=document.getElementById('loadState');const b=document.getElementById('toggleFullscreen');if(b){b.onclick=()=>{document.body.classList.toggle('fullscreen-active');if(document.body.classList.contains('fullscreen-active')){b.textContent='退出全屏';try{document.documentElement.requestFullscreen()}catch{}}else{b.textContent='全屏沉浸';try{if(document.fullscreenElement)document.exitFullscreen()}catch{}}};document.addEventListener('fullscreenchange',()=>{if(!document.fullscreenElement){document.body.classList.remove('fullscreen-active');b.textContent='全屏沉浸';}});}try{const response=await fetch(__BODY_URL__,{credentials:'omit',cache:'no-store'});if(!response.ok)throw Error();const data=await response.json();if(data.visibility!=='public')throw Error();if(data.content_type==='html'){const frame=document.getElementById('publicFrame');frame.srcdoc='<meta http-equiv="Content-Security-Policy" content="default-src \'none\'; style-src \'unsafe-inline\'; img-src data: blob:">'+data.content;frame.hidden=false;}else{const pre=document.getElementById('publicText');pre.textContent=data.content;pre.hidden=false;}status.hidden=true;status.remove();}catch{status.textContent='内容不存在、已失效或暂时无法加载，请稍后刷新重试。';}})();</script>"#.replace("__BODY_URL__", &body_url);
    shell_with_class(
        title,
        "viewer-immersive",
        &format!(
            r#"<div class="compact-bar"><div class="compact-bar-left"><a class="compact-brand" href="/" title="FlashPaste 首页">↗</a><span class="compact-title">{}</span><span class="compact-badge">HTML 静态预览</span></div><div class="compact-bar-right"><a class="compact-btn" href="/d/{}" download>下载原件</a><button class="compact-btn" id="toggleFullscreen" type="button" title="切换全屏沉浸浏览">全屏沉浸</button><a class="compact-btn quiet" href="/">工作台</a></div></div><div class="top-trigger" aria-hidden="true"></div><div class="viewer-container"><p id="loadState" class="viewer-loading" role="status">正在加载内容…</p><iframe id="publicFrame" title="分享内容" sandbox="" hidden></iframe><pre id="publicText" hidden></pre></div>{script}"#,
            escape(title),
            id_escaped
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_templates_escape_user_text_and_keep_html_sandboxed() {
        let html = public_view(
            Some("<script>bad</script>"),
            "html",
            "<script>alert(1)</script>",
        );
        assert!(html.contains("&lt;script&gt;bad&lt;/script&gt;"));
        assert!(html.contains("sandbox=\"\""));
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(!html.contains("<script>bad</script>"));
        let text = public_view(None, "text", "<img src=x onerror=alert(1)>");
        assert!(text.contains("无标题分享"));
        assert!(text.contains("&lt;img"));
        assert!(!text.contains("<img src=x"));
        let literal = public_view(Some("{{CONTENT}}"), "text", "{{TITLE}}");
        assert!(literal.contains("<title>{{CONTENT}} · FlashPaste</title>"));
        assert!(literal.contains("<pre>{{TITLE}}</pre>"));
    }
}
