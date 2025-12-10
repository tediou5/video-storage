#version 330 core

in vec2 TexCoord;

out vec4 FragColor;

uniform sampler2D texture1;

uniform float playTime;       // Play time in seconds
uniform int width;            // Frame width
uniform int height;           // Frame height

void main() {
    float duration = 0.9;
    float maxAlpha = 0.1;
    float maxScale = 1.5;

    float progress = mod(playTime, duration) / duration;
    float alpha = maxAlpha * (1.0 - progress);
    float scale = 1.0 + (maxScale - 1.0) * progress;

    float weakX = 0.5 + (TexCoord.x - 0.5) / scale;
    float weakY = 0.5 + (TexCoord.y - 0.5) / scale;

    vec2 weakTextureCoords = vec2(weakX, weakY);
    vec4 weakMask = texture(texture1, weakTextureCoords);

    vec4 mask = texture(texture1, TexCoord);

    FragColor = mask * (1.0 - alpha) + weakMask * alpha;
}