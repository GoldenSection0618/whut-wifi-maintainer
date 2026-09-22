include $(TOPDIR)/rules.mk

# Generated from the committed source by scripts/build_openwrt.py.
include $(CURDIR)/source.mk

PKG_NAME:=whut-wifi-maintainer
PKG_VERSION:=$(WHUT_VERSION)
PKG_RELEASE:=1
PKG_SOURCE:=$(PKG_NAME)-$(PKG_VERSION).tar.gz
PKG_HASH:=$(WHUT_SOURCE_SHA256)
PKG_LICENSE:=MIT
PKG_LICENSE_FILES:=LICENSE
PKG_BUILD_PARALLEL:=1

ifneq ($(WHUT_SYSTEM_RUST),1)
PKG_BUILD_DEPENDS:=rust/host
endif

include $(INCLUDE_DIR)/package.mk
include $(TOPDIR)/feeds/packages/lang/rust/rust-package.mk

define Package/whut-wifi-maintainer
  SECTION:=net
  CATEGORY:=Network
  TITLE:=WHUT campus network keepalive
  URL:=https://github.com/GoldenSection0618/whut-wifi-maintainer
  DEPENDS:=$(RUST_ARCH_DEPENDS) +libc +libgcc +ca-bundle
endef

define Package/whut-wifi-maintainer/description
  Explicit-interface campus authentication with HTTP and HTTPS health probes.
endef

define Package/whut-wifi-maintainer/conffiles
/etc/whut-wifi-maintainer/config.toml
endef

define Package/whut-wifi-maintainer/install
	$(INSTALL_DIR) $(1)/usr/bin $(1)/etc/init.d $(1)/etc/whut-wifi-maintainer $(1)/lib/upgrade/keep.d
	$(INSTALL_BIN) $(PKG_INSTALL_DIR)/bin/whut-wifi-maintainer $(1)/usr/bin/
	$(INSTALL_BIN) ./files/whut-wifi-maintainer.init $(1)/etc/init.d/whut-wifi-maintainer
	$(INSTALL_CONF) ./files/config.toml.example $(1)/etc/whut-wifi-maintainer/config.toml
	$(INSTALL_DATA) ./files/whut-wifi-maintainer.keep $(1)/lib/upgrade/keep.d/whut-wifi-maintainer
endef

$(eval $(call BuildPackage,whut-wifi-maintainer))
