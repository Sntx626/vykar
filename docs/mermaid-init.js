// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

(() => {
    const darkThemes = ['ayu', 'navy', 'coal'];
    const lightThemes = ['light', 'rust'];

    const classList = document.getElementsByTagName('html')[0].classList;

    let lastThemeWasLight = true;
    for (const cssClass of classList) {
        if (darkThemes.includes(cssClass)) {
            lastThemeWasLight = false;
            break;
        }
    }

    const theme = lastThemeWasLight ? 'base' : 'dark';
    const themeVariables = lastThemeWasLight
        ? {
            fontSize: '13px',
            primaryColor: '#fff3e0',
            primaryBorderColor: '#ffb74d',
            primaryTextColor: '#333',
            lineColor: '#999',
            clusterBkg: '#f5f5f5',
            clusterBorder: '#ccc',
        }
        : {
            fontSize: '13px',
            primaryColor: '#4a3520',
            primaryBorderColor: '#ffb74d',
            primaryTextColor: '#e0e0e0',
            lineColor: '#888',
            clusterBkg: '#2a2a2a',
            clusterBorder: '#555',
        };
    mermaid.initialize({
        startOnLoad: true,
        theme,
        themeVariables,
        flowchart: {
            useMaxWidth: true,
            nodeSpacing: 18,
            rankSpacing: 24,
            padding: 4,
        },
    });

    // Simplest way to make mermaid re-render the diagrams in the new theme is via refreshing the page

    for (const darkTheme of darkThemes) {
        document.getElementById(darkTheme).addEventListener('click', () => {
            if (lastThemeWasLight) {
                window.location.reload();
            }
        });
    }

    for (const lightTheme of lightThemes) {
        document.getElementById(lightTheme).addEventListener('click', () => {
            if (!lastThemeWasLight) {
                window.location.reload();
            }
        });
    }
})();
