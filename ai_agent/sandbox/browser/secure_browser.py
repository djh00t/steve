"""
Secure browser implementation with anti-detection measures.
"""
import asyncio
import random
import logging
from typing import Dict, Any, Optional, List
from pathlib import Path
import json
import time

from selenium import webdriver
from selenium.webdriver import Chrome, ChromeOptions
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import WebDriverWait
from selenium.webdriver.support import expected_conditions as EC
from selenium.common.exceptions import TimeoutException
from fake_useragent import UserAgent

logger = logging.getLogger(__name__)

class BrowserConfig:
    """Browser configuration with anti-detection measures."""
    
    def __init__(self):
        self.user_agent = UserAgent()
        self._load_profiles()
        
    def _load_profiles(self):
        """Load browser profiles and configurations."""
        self.profiles = {
            'default': {
                'window_size': (1920, 1080),
                'viewport_size': (1920, 1080),
                'languages': ['en-US', 'en'],
                'platform': 'Linux x86_64',
                'webgl_vendor': 'Mesa/X.org',
                'renderer': 'Mesa Intel(R) UHD Graphics (CML GT2)',
                'timezone': 'America/New_York'
            }
        }
        
    def get_chrome_options(self, profile: str = 'default') -> ChromeOptions:
        """Get Chrome options with anti-detection measures."""
        options = ChromeOptions()
        profile_data = self.profiles[profile]
        
        # Basic settings
        options.add_argument('--no-sandbox')
        options.add_argument('--disable-dev-shm-usage')
        options.add_argument('--disable-gpu')
        options.add_argument(f'--window-size={profile_data["window_size"][0]},{profile_data["window_size"][1]}')
        
        # Anti-detection settings
        options.add_argument(f'--user-agent={self.user_agent.random}')
        options.add_argument('--disable-blink-features=AutomationControlled')
        options.add_experimental_option('excludeSwitches', ['enable-automation'])
        options.add_experimental_option('useAutomationExtension', False)
        
        # Additional preferences
        options.add_experimental_option('prefs', {
            'intl.accept_languages': profile_data['languages'],
            'profile.default_content_setting_values.notifications': 2,
            'profile.default_content_setting_values.geolocation': 2,
            'profile.managed_default_content_settings.images': 1,
            'profile.default_content_setting_values.cookies': 1
        })
        
        return options

class SecureBrowser:
    """Secure browser implementation with anti-detection measures."""
    
    def __init__(self):
        """Initialize secure browser."""
        self.config = BrowserConfig()
        self.browser: Optional[Chrome] = None
        self._setup_complete = False
        
    async def setup(self):
        """Set up browser instance."""
        if not self._setup_complete:
            options = self.config.get_chrome_options()
            self.browser = Chrome(options=options)
            
            # Execute anti-detection scripts
            await self._setup_anti_detection()
            self._setup_complete = True
            
    async def _setup_anti_detection(self):
        """Set up additional anti-detection measures."""
        if not self.browser:
            return
            
        # Override navigator properties
        script = """
        Object.defineProperty(navigator, 'webdriver', {
            get: () => undefined
        });
        
        Object.defineProperty(navigator, 'languages', {
            get: () => ['en-US', 'en']
        });
        
        Object.defineProperty(navigator, 'plugins', {
            get: () => [
                {
                    0: {type: "application/x-google-chrome-pdf"},
                    description: "Portable Document Format",
                    filename: "internal-pdf-viewer",
                    length: 1,
                    name: "Chrome PDF Plugin"
                }
            ]
        });
        """
        
        self.browser.execute_script(script)
        
    async def browse(
        self,
        url: str,
        timeout: int = 30,
        wait_for: Optional[str] = None
    ) -> Dict[str, Any]:
        """
        Browse to a URL with anti-detection measures.
        
        Args:
            url: URL to browse
            timeout: Page load timeout in seconds
            wait_for: Optional CSS selector to wait for
            
        Returns:
            Dict containing page data
        """
        if not self._setup_complete:
            await self.setup()
            
        try:
            # Add random delay
            await asyncio.sleep(random.uniform(1, 3))
            
            # Navigate with retry logic
            for attempt in range(3):
                try:
                    self.browser.get(url)
                    break
                except Exception as e:
                    if attempt == 2:
                        raise
                    await asyncio.sleep(random.uniform(2, 5))
                    
            # Wait for content
            if wait_for:
                WebDriverWait(self.browser, timeout).until(
                    EC.presence_of_element_located((By.CSS_SELECTOR, wait_for))
                )
            else:
                # Wait for page load
                WebDriverWait(self.browser, timeout).until(
                    lambda driver: driver.execute_script('return document.readyState') == 'complete'
                )
                
            # Add random scroll behavior
            await self._random_scroll()
            
            # Extract content
            content = await self._extract_content()
            
            return {
                'url': self.browser.current_url,
                'title': self.browser.title,
                'content': content,
                'success': True
            }
            
        except Exception as e:
            logger.error(f"Error browsing {url}: {str(e)}")
            return {
                'url': url,
                'error': str(e),
                'success': False
            }
            
    async def _random_scroll(self):
        """Perform random scrolling behavior."""
        if not self.browser:
            return
            
        # Get page height
        height = self.browser.execute_script('return document.body.scrollHeight')
        
        # Random scroll positions
        positions = sorted([
            random.randint(0, height)
            for _ in range(random.randint(3, 7))
        ])
        
        # Scroll with random delays
        for position in positions:
            self.browser.execute_script(f'window.scrollTo(0, {position})')
            await asyncio.sleep(random.uniform(0.5, 2))
            
    async def _extract_content(self) -> Dict[str, Any]:
        """Extract content from current page."""
        if not self.browser:
            return {}
            
        # Basic content extraction
        content = {
            'text': self.browser.find_element(By.TAG_NAME, 'body').text,
            'links': [
                {
                    'text': el.text,
                    'href': el.get_attribute('href')
                }
                for el in self.browser.find_elements(By.TAG_NAME, 'a')
                if el.get_attribute('href')
            ],
            'metadata': {
                'title': self.browser.title,
                'meta_description': '',
                'meta_keywords': ''
            }
        }
        
        # Extract metadata
        try:
            meta_desc = self.browser.find_element(
                By.CSS_SELECTOR,
                'meta[name="description"]'
            )
            content['metadata']['meta_description'] = meta_desc.get_attribute('content')
        except:
            pass
            
        try:
            meta_keywords = self.browser.find_element(
                By.CSS_SELECTOR,
                'meta[name="keywords"]'
            )
            content['metadata']['meta_keywords'] = meta_keywords.get_attribute('content')
        except:
            pass
            
        return content
        
    async def close(self):
        """Close browser instance."""
        if self.browser:
            self.browser.quit()
            self.browser = None
            self._setup_complete = False
            
    async def __aenter__(self):
        """Context manager entry."""
        await self.setup()
        return self
        
    async def __aexit__(self, exc_type, exc_val, exc_tb):
        """Context manager exit."""
        await self.close()